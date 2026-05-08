/**
 * Shared utility functions for the dBrain OpenCode plugin.
 */

export function truncate(value: string, max: number): string {
  if (!value) return '';
  return value.length > max ? value.slice(0, max) + '...' : value;
}
