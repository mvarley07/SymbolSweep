/** Format bytes to human-readable string (matches Rust format_size) */
export function formatSize(bytes: number): string {
  const KB = 1024;
  const MB = KB * 1024;
  const GB = MB * 1024;
  const GB_THRESHOLD = 1000 * MB;

  if (bytes >= GB_THRESHOLD) {
    const value = bytes / GB;
    const rounded = Math.round(value * 10) / 10;
    if (Math.abs(rounded - Math.floor(rounded)) < 0.01) {
      return `${Math.floor(rounded)} GB`;
    }
    return `${rounded.toFixed(1)} GB`;
  } else if (bytes >= MB) {
    return `${Math.floor(bytes / MB)} MB`;
  } else if (bytes >= KB) {
    return `${Math.floor(bytes / KB)} KB`;
  } else if (bytes > 0) {
    return `${bytes} B`;
  }
  return '0 B';
}
