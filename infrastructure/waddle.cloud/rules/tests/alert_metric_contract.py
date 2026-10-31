"""Validate clustering alert dependencies using Prometheus's PromQL AST."""


def metric_selectors(node):
    """Return exact metric names read by a parsed expression, or fail closed."""
    if not isinstance(node, dict) or not isinstance(node.get("type"), str):
        raise AssertionError(f"unexpected PromQL AST node: {node!r}")
    kind = node["type"]
    if kind in ("vectorSelector", "matrixSelector"):
        matchers = node.get("matchers")
        if not isinstance(matchers, list):
            raise AssertionError(f"selector has no matcher list: {node!r}")
        names = [matcher for matcher in matchers if matcher.get("name") == "__name__"]
        # A broad or regex selector could include a clustering metric that
        # isn't named in the expression. Require an exact name so the
        # contract cannot silently overlook one.
        if len(names) != 1 or names[0].get("type") != "=" or not names[0].get("value"):
            raise AssertionError(f"selector must use one exact metric name: {node!r}")
        name = names[0]["value"]
        if node.get("name") and node["name"] != name:
            raise AssertionError(f"selector name and matcher disagree: {node!r}")
        return {name}
    if kind in ("numberLiteral", "stringLiteral"):
        return set()
    if kind in ("aggregation", "parenExpr", "subquery", "unaryExpr"):
        children = [node.get("expr")]
        if kind == "aggregation" and node.get("param") is not None:
            children.append(node["param"])
    elif kind == "binaryExpr":
        children = [node.get("lhs"), node.get("rhs")]
    elif kind == "call":
        children = node.get("args")
        if not isinstance(children, list):
            raise AssertionError(f"function call has no argument list: {node!r}")
    else:
        # The API is experimental. An unknown AST shape must not turn a
        # newly introduced expression into a passing empty selector set.
        raise AssertionError(f"unsupported PromQL AST node {kind!r}")
    names = set()
    for child in children:
        names.update(metric_selectors(child))
    return names


def validate_rule_contract(groups, contracts, parse_query):
    if not groups or not contracts:
        raise AssertionError("alert rules and clustering metric contracts must not be empty")
    allowed = set()
    for contract in contracts:
        allowed.add(contract["prometheus_name"])
        allowed.update(contract["unexported_rule_spellings"])
    observed = set()
    alerts = {}
    for group in groups:
        for rule in group["rules"]:
            names = metric_selectors(parse_query(rule["query"]))
            observed.update(names)
            for name in names:
                if name.startswith("waddle_clustering_") and name not in allowed:
                    raise AssertionError(f"{name} is read by a rule but has no clustering metric contract")
            if rule["type"] == "alerting":
                alerts.setdefault(rule["name"], []).append(names)
    for contract in contracts:
        if not contract["alerts"]:
            raise AssertionError(f"{contract['name']} names no alert")
        for alert in contract["alerts"]:
            matches = alerts.get(alert, [])
            if len(matches) != 1:
                raise AssertionError(f"expected one alert named {alert}, found {len(matches)}")
            if contract["prometheus_name"] not in matches[0]:
                raise AssertionError(f"alert {alert} no longer reads {contract['prometheus_name']}")
        for spelling in contract["unexported_rule_spellings"]:
            if spelling not in observed:
                raise AssertionError(f"{spelling} is no longer read by a rule; drop it from the contract")


def exercise_parser_controls(parse_query):
    """Prove real Prometheus selectors, comments and literals are distinguished."""
    metric = "waddle_clustering_contract_probe"
    contract = [{"name": "probe", "prometheus_name": metric, "alerts": ["Probe"],
                 "unexported_rule_spellings": []}]

    def check(expression):
        groups = [{"rules": [{"type": "alerting", "name": "Probe", "query": expression}]}]
        validate_rule_contract(groups, contract, parse_query)

    for expression in (metric, '{__name__="' + metric + '"}', 'rate(' + metric + '[5m])'):
        check(expression)
    for expression in ('up # ' + metric, 'up{note="' + metric + '"}',
                       'sum by (' + metric + ') (up)',
                       'label_replace(up, "dst", "' + metric + '", "src", ".*")'):
        try:
            check(expression)
        except AssertionError as error:
            if "no longer reads" not in str(error):
                raise
        else:
            raise AssertionError(f"non-selector satisfied alert contract: {expression}")
    try:
        check('{__name__=~"waddle_clustering_.*"}')
    except AssertionError as error:
        if "exact metric name" not in str(error):
            raise
    else:
        raise AssertionError("broad selector bypassed clustering metric contract")
