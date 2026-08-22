/**
 * A validator for the JSON Schema subset the agent's tool definitions use.
 *
 * Tool arguments arrive from a language model, so validation is a hot path on every call and its
 * error messages are read by the model rather than by a person. Both of those argue for a small
 * exact implementation over a general one: the messages name the offending path and say what was
 * expected, and defaults are filled in so a handler never sees a half-populated object.
 */

export interface ValidationResult<T = unknown> {
  valid: boolean;
  errors: string[];
  value: T;
}

type Schema = Record<string, any>;

export function validate<T = unknown>(schema: Schema, input: unknown): ValidationResult<T> {
  const errors: string[] = [];
  const value = walk(schema, input, "", errors);
  return { valid: errors.length === 0, errors, value: value as T };
}

function walk(schema: Schema, input: unknown, path: string, errors: string[]): unknown {
  const at = path || "(root)";

  if (schema.const !== undefined && input !== schema.const) {
    errors.push(`${at}: must be ${JSON.stringify(schema.const)}`);
    return input;
  }

  if (Array.isArray(schema.enum) && !schema.enum.includes(input as never)) {
    errors.push(`${at}: must be one of ${schema.enum.map((v: unknown) => JSON.stringify(v)).join(", ")}`);
    return input;
  }

  const types: string[] = schema.type === undefined ? [] : Array.isArray(schema.type) ? schema.type : [schema.type];
  if (types.length > 0 && !types.some((type) => matchesType(type, input))) {
    errors.push(`${at}: expected ${types.join(" or ")}, got ${describe(input)}`);
    return input;
  }

  if (typeof input === "string") {
    if (typeof schema.minLength === "number" && input.length < schema.minLength) {
      errors.push(`${at}: must be at least ${schema.minLength} characters`);
    }
    if (typeof schema.maxLength === "number" && input.length > schema.maxLength) {
      errors.push(`${at}: must be at most ${schema.maxLength} characters`);
    }
    if (typeof schema.pattern === "string" && !new RegExp(schema.pattern).test(input)) {
      errors.push(`${at}: must match ${schema.pattern}`);
    }
  }

  if (typeof input === "number") {
    if (typeof schema.minimum === "number" && input < schema.minimum) {
      errors.push(`${at}: must be >= ${schema.minimum}`);
    }
    if (typeof schema.maximum === "number" && input > schema.maximum) {
      errors.push(`${at}: must be <= ${schema.maximum}`);
    }
  }

  if (Array.isArray(input)) {
    if (typeof schema.minItems === "number" && input.length < schema.minItems) {
      errors.push(`${at}: needs at least ${schema.minItems} item${schema.minItems === 1 ? "" : "s"}`);
    }
    if (typeof schema.maxItems === "number" && input.length > schema.maxItems) {
      errors.push(`${at}: allows at most ${schema.maxItems} items`);
    }
    if (schema.items) {
      return input.map((item, index) => walk(schema.items, item, `${path}[${index}]`, errors));
    }
    return input;
  }

  if (isPlainObject(input)) {
    const properties: Record<string, Schema> = schema.properties ?? {};
    const output: Record<string, unknown> = {};

    for (const key of schema.required ?? []) {
      if (!(key in input)) errors.push(`${at}: missing required property "${key}"`);
    }

    for (const [key, raw] of Object.entries(input)) {
      const child = properties[key];
      if (!child) {
        if (schema.additionalProperties === false) {
          const known = Object.keys(properties);
          errors.push(
            `${at}: unexpected property "${key}"${known.length > 0 ? `; allowed: ${known.join(", ")}` : ""}`,
          );
        } else {
          output[key] = raw;
        }
        continue;
      }
      output[key] = walk(child, raw, path ? `${path}.${key}` : key, errors);
    }

    for (const [key, child] of Object.entries(properties)) {
      if (!(key in output) && child.default !== undefined) output[key] = child.default;
    }

    return output;
  }

  return input;
}

function matchesType(type: string, value: unknown): boolean {
  switch (type) {
    case "string":
      return typeof value === "string";
    case "number":
      return typeof value === "number" && Number.isFinite(value);
    case "integer":
      return typeof value === "number" && Number.isInteger(value);
    case "boolean":
      return typeof value === "boolean";
    case "array":
      return Array.isArray(value);
    case "object":
      return isPlainObject(value);
    case "null":
      return value === null;
    default:
      return true;
  }
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function describe(value: unknown): string {
  if (value === null) return "null";
  if (Array.isArray(value)) return "array";
  return typeof value;
}
