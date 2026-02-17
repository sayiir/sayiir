/**
 * Duration parsing utility.
 *
 * Converts human-readable duration strings (via `ms` library) or numeric
 * milliseconds to milliseconds.
 */

import type { Duration } from "./types.js";

let _ms: ((val: string) => number | undefined) | undefined;

function getMs(): (val: string) => number | undefined {
  if (!_ms) {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    _ms = require("ms") as (val: string) => number | undefined;
  }
  return _ms;
}

/**
 * Parse a Duration value to milliseconds.
 *
 * - Numbers are returned as-is (assumed to be ms).
 * - Strings are parsed via the `ms` library (e.g. "30s", "5m", "1h").
 */
export function parseDuration(d: Duration): number {
  if (typeof d === "number") return d;
  const result = getMs()(d);
  if (result == null) {
    throw new Error(`Invalid duration: "${d}"`);
  }
  return result;
}
