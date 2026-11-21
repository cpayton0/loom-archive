/**
 * Share or embed links, optionally with a title slug. This only decides what
 * the UI offers; Rust's `normalize_share_url` is the authoritative check.
 */
export const shareLink = /^https:\/\/(www\.)?loom\.com\/(share|embed)\/([\w.%-]+-)?([0-9a-f]{32})\/?([?#]\S*)?$/i;

/** Removes the backslashes shells add before `?` and `=` when copying links. */
export const cleanLink = (text: string) => text.trim().replace(/\\/g, "");

/** The recording ID of a share link, or null when the text is not one. */
export function recordingId(text: string): string | null {
  return shareLink.exec(cleanLink(text))?.[4].toLowerCase() ?? null;
}

export interface ListedLink { url: string; id: string | null; duplicate: boolean; }

/** One entry per non-empty, non-comment line, flagging invalid and repeated links. */
export function parseLinks(text: string): ListedLink[] {
  const seen = new Set<string>();
  return text.split(/\r?\n/).map(cleanLink).filter(l => l && !l.startsWith("#")).map(url => {
    const id = recordingId(url);
    const duplicate = id !== null && seen.has(id);
    if (id) seen.add(id);
    return { url, id, duplicate };
  });
}
