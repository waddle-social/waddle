// Resolves Panda CSS token definitions outside Panda.
//
// Covers the subset of Panda's token format that chat/panda.config.ts uses:
//   - nested groups flattened to dotted names (`teal.300`, `bg.rail`), with
//     the `DEFAULT` key naming the group itself (`fg.DEFAULT` -> `fg`);
//   - references `{category.name}` to tokens or semantic tokens, whole-value
//     or embedded, followed through chains;
//   - semantic values that are plain or keyed by condition (`_dark`,
//     `_light`, `base`), picked per scheme.
// Anything else (missing references, cycles, nested conditions, a scheme
// with no value) throws: the generator must fail loudly, never guess.

const REFERENCE = /\{([^{}]+)\}/g;

function isLeaf(node) {
  return node !== null && typeof node === "object" && Object.hasOwn(node, "value");
}

/**
 * Flattens a token tree into `Map<dottedName, value>`. `DEFAULT` names the
 * enclosing group. Throws on duplicate names (`"bg.rail"` and `bg.rail`).
 */
export function flattenTokens(tree, prefix = [], out = new Map()) {
  for (const [key, node] of Object.entries(tree ?? {})) {
    const path = key === "DEFAULT" ? prefix : [...prefix, key];
    if (isLeaf(node)) {
      const name = path.join(".");
      if (name === "") throw new Error("token without a name (top-level DEFAULT)");
      if (out.has(name)) throw new Error(`token "${name}" is defined twice`);
      out.set(name, node.value);
    } else if (node !== null && typeof node === "object") {
      flattenTokens(node, path, out);
    } else {
      throw new Error(`token group "${path.join(".")}" has a non-object entry`);
    }
  }
  return out;
}

/**
 * Builds a resolver over `{ tokens, semanticTokens }` (Panda's
 * `theme.tokens` / `theme.semanticTokens` shape). `conditions` maps a scheme
 * name to the condition key that selects it, e.g.
 * `{ night: "_dark", daylight: "_light" }`.
 */
export function createPandaTokenResolver({ tokens, semanticTokens, conditions }) {
  const plain = new Map();
  const semantic = new Map();
  for (const [category, tree] of Object.entries(tokens ?? {})) {
    plain.set(category, flattenTokens(tree));
  }
  for (const [category, tree] of Object.entries(semanticTokens ?? {})) {
    semantic.set(category, flattenTokens(tree));
  }
  for (const [category, names] of semantic) {
    for (const name of names.keys()) {
      if (plain.get(category)?.has(name)) {
        throw new Error(`${category}.${name} is both a token and a semantic token`);
      }
    }
  }

  function conditionKey(scheme) {
    const key = conditions?.[scheme];
    if (!key) throw new Error(`unknown scheme "${scheme}"`);
    return key;
  }

  function pickCondition(ref, raw, scheme) {
    if (raw === null || typeof raw !== "object") return raw;
    const key = conditionKey(scheme);
    const picked = Object.hasOwn(raw, key) ? raw[key] : raw.base;
    if (picked === undefined) {
      throw new Error(`${ref}: no "${key}" or "base" value for scheme "${scheme}"`);
    }
    if (picked !== null && typeof picked === "object") {
      throw new Error(`${ref}: nested conditions are not supported`);
    }
    return picked;
  }

  function lookup(category, name, scheme, stack) {
    const ref = `${category}.${name}`;
    if (stack.includes(ref)) {
      throw new Error(`reference cycle: ${[...stack, ref].join(" -> ")}`);
    }
    const next = [...stack, ref];
    let raw;
    if (semantic.get(category)?.has(name)) {
      raw = pickCondition(ref, semantic.get(category).get(name), scheme);
    } else if (plain.get(category)?.has(name)) {
      raw = plain.get(category).get(name);
    } else {
      const from = stack.length ? ` (from ${stack.at(-1)})` : "";
      throw new Error(`unknown token reference {${ref}}${from}`);
    }
    return substitute(raw, scheme, next);
  }

  function substitute(raw, scheme, stack) {
    if (typeof raw !== "string") return raw;
    const whole = /^\{([^{}]+)\}$/.exec(raw);
    if (whole) return lookupPath(whole[1], scheme, stack);
    return raw.replace(REFERENCE, (_, path) => String(lookupPath(path, scheme, stack)));
  }

  function lookupPath(path, scheme, stack) {
    const dot = path.indexOf(".");
    if (dot <= 0) throw new Error(`malformed token reference {${path}}`);
    return lookup(path.slice(0, dot), path.slice(dot + 1), scheme, stack);
  }

  return {
    /** Resolved value of `category.name` (token or semantic) for `scheme`. */
    resolve(category, name, scheme) {
      return lookup(category, name, scheme, []);
    },
    /** Flattened semantic token names of `category`, in declaration order. */
    semanticNames(category) {
      return [...(semantic.get(category)?.keys() ?? [])];
    },
    /** Flattened token names of `category`, in declaration order. */
    tokenNames(category) {
      return [...(plain.get(category)?.keys() ?? [])];
    },
  };
}
