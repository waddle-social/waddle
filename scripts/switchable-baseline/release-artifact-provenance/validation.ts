import {
  boundedLabelPattern,
  requireSha256,
} from "../gate-evidence/common";

export function exactLiteral(
  value: unknown,
  expected: string,
  label: string,
): void {
  if (value !== expected) throw new Error(`${label} must be ${expected}`);
}

export function requireNonPlaceholderSha256(
  value: unknown,
  label: string,
): string {
  const digest = requireSha256(value, label);
  if (/^([0-9a-f])\1{63}$/.test(digest)) {
    throw new Error(`${label} must not be a placeholder digest`);
  }
  return digest;
}

export function requireOciDigest(value: unknown, label: string): string {
  if (typeof value !== "string" || !/^sha256:[0-9a-f]{64}$/.test(value)) {
    throw new Error(`${label} must be a sha256 OCI digest`);
  }
  requireNonPlaceholderSha256(value.slice("sha256:".length), label);
  return value;
}

export function requireBoundedLabel(value: unknown, label: string): string {
  if (
    typeof value !== "string"
    || value === "unknown"
    || !boundedLabelPattern.test(value)
  ) {
    throw new Error(`${label} must be a bounded deployment label`);
  }
  return value;
}
