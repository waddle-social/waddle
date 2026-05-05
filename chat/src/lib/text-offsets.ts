export function codePointLength(input: string): number {
  return Array.from(input).length;
}

function codePointToCodeUnitIndex(input: string, codePointOffset: number): number {
  if (codePointOffset <= 0) return 0;
  let codePoints = 0;
  let codeUnits = 0;
  for (const char of input) {
    if (codePoints >= codePointOffset) break;
    codePoints++;
    codeUnits += char.length;
  }
  return codeUnits;
}

export function sliceByCodePoints(input: string, start: number, end = codePointLength(input)): string {
  return input.slice(
    codePointToCodeUnitIndex(input, start),
    codePointToCodeUnitIndex(input, end),
  );
}
