import type {
  ExtensionCommandAction,
  ExtensionCommandActions,
  ExtensionCommandFormField,
  ExtensionCommandFormOption,
  FormFieldLike,
} from "./types";

export function parseExtensionCommandForm(form: unknown): ExtensionCommandFormField[] {
  const fields = (form as { fields?: unknown[] } | undefined)?.fields;
  if (!Array.isArray(fields)) return [];
  return fields.flatMap((field) => {
    const item = field as {
      name?: unknown;
      var?: unknown;
      label?: unknown;
      type?: unknown;
      desc?: unknown;
      description?: unknown;
      value?: unknown;
      values?: unknown[];
      rawValues?: unknown[];
      required?: unknown;
      options?: unknown[];
    };
    const name = typeof item.name === "string" ? item.name : typeof item.var === "string" ? item.var : "";
    const type = typeof item.type === "string" ? item.type : "text-single";
    if (!name && type !== "fixed") return [];
    const values = Array.isArray(item.value)
      ? item.value
      : Array.isArray(item.values)
        ? item.values
        : Array.isArray(item.rawValues)
          ? item.rawValues
          : item.value !== undefined
            ? [item.value]
            : [];
    const stringValues = values
      .filter((value) => typeof value === "string" || typeof value === "number" || typeof value === "boolean")
      .map((value) => String(value));
    const fieldValues = type === "boolean" && stringValues.length === 0 ? ["0"] : stringValues;
    const fieldName = name || `fixed:${fieldValues.join("\n")}`;
    return [{
      name: fieldName,
      label: typeof item.label === "string" && item.label ? item.label : fieldName,
      type,
      ...(typeof item.desc === "string" && item.desc ? { description: item.desc } : {}),
      ...(typeof item.description === "string" && item.description ? { description: item.description } : {}),
      value: fieldValues[0] ?? "",
      values: fieldValues,
      options: parseFieldOptions(item.options),
      required: item.required === true,
      blocked: isForbiddenExtensionCommandField(name, type),
      hidden: type === "hidden",
    }];
  });
}

export function visibleExtensionCommandFields(fields: ExtensionCommandFormField[]): ExtensionCommandFormField[] {
  return fields.filter((field) => !field.hidden);
}

export function extensionCommandFormBlockedReason(fields: ExtensionCommandFormField[]): string | undefined {
  const blocked = fields.find((field) => field.blocked);
  if (!blocked) return undefined;
  return `Extension command form contains a forbidden field: ${blocked.label}.`;
}

function hasRequiredExtensionCommandValue(field: ExtensionCommandFormField): boolean {
  if (field.type === "fixed") return true;
  if (field.type === "boolean") return field.value.trim().length > 0;
  if (field.type === "list-multi" || field.type === "text-multi" || field.type === "jid-multi") {
    return field.values.some((value) => value.trim().length > 0);
  }
  return field.value.trim().length > 0;
}

export function missingRequiredExtensionCommandFields(fields: ExtensionCommandFormField[]): ExtensionCommandFormField[] {
  return visibleExtensionCommandFields(fields).filter((field) => field.required && !hasRequiredExtensionCommandValue(field));
}

function parseFieldOptions(options: unknown): ExtensionCommandFormOption[] {
  if (!Array.isArray(options)) return [];
  return options.flatMap((option) => {
    const item = option as { label?: unknown; value?: unknown; values?: unknown[] };
    const rawValue = item.value ?? item.values?.[0];
    if (typeof rawValue !== "string" && typeof rawValue !== "number" && typeof rawValue !== "boolean") return [];
    const value = String(rawValue);
    return [{
      label: typeof item.label === "string" && item.label ? item.label : value,
      value,
    }];
  });
}

export function dataFormFieldValue(field: ExtensionCommandFormField): string | string[] | boolean {
  if (field.type === "boolean") return field.value === "1" || field.value === "true";
  if (field.type === "hidden" && field.values.length > 1) return field.values;
  if (field.type === "list-multi" || field.type === "text-multi" || field.type === "jid-multi") {
    return field.values.length > 0 ? field.values : [];
  }
  return field.value;
}

export function parseCommandActions(actions: unknown, status?: string, actionsProvided = false): ExtensionCommandActions | undefined {
  const value = (actions && typeof actions === "object" ? actions : {}) as {
    execute?: unknown;
    next?: unknown;
    prev?: unknown;
    previous?: unknown;
    complete?: unknown;
    cancel?: unknown;
    allowed?: unknown[];
  };
  const allowed = new Set<ExtensionCommandAction>();
  if (actions && typeof actions === "object") {
    if (Array.isArray(value.allowed)) {
      for (const action of value.allowed) {
        if (isExtensionCommandAction(action)) allowed.add(action);
      }
    }
    if (value.next !== undefined) allowed.add("next");
    if (value.prev !== undefined || value.previous !== undefined) allowed.add("prev");
    if (value.complete !== undefined) allowed.add("complete");
    if (value.cancel !== undefined) allowed.add("cancel");
  }
  const execute = isExtensionCommandAction(value.execute) ? value.execute : undefined;
  if (execute) allowed.add(execute);
  if (status === "executing" && !actionsProvided) allowed.add("complete");
  if (status === "executing") allowed.add("cancel");
  const allowedList = [...allowed];
  return allowedList.length > 0 || execute ? { ...(execute ? { execute } : {}), allowed: allowedList } : undefined;
}

function isExtensionCommandAction(value: unknown): value is ExtensionCommandAction {
  return value === "next" || value === "prev" || value === "complete" || value === "cancel";
}

function isForbiddenExtensionCommandField(name: string, type: string): boolean {
  if (type === "text-private") return true;
  return /(?:^|[#:_-])(secret|token|password|api[_-]?key|apikey|credential)(?:$|[#:_-])/i.test(name);
}

export function formFieldValue(fields: unknown[], name: string): string | null {
  const field = fields
    .map((value) => value as FormFieldLike)
    .find((value) => (typeof value.name === "string" ? value.name : value.var) === name);
  const values = formFieldValues(field);
  return values[0] ?? null;
}

function formFieldValues(field: FormFieldLike | undefined): string[] {
  if (!field) return [];
  const values = Array.isArray(field.value)
    ? field.value
    : Array.isArray(field.values)
      ? field.values
      : Array.isArray(field.rawValues)
        ? field.rawValues
        : field.value !== undefined
          ? [field.value]
          : [];
  return values
    .filter((value) => typeof value === "string" || typeof value === "number" || typeof value === "boolean")
    .map((value) => String(value));
}
