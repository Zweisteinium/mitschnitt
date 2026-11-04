import { api } from "./api";

const cache = new Map<string, Promise<string | null>>();

/** The cover a file carries, read once per file. */
export function coverOf(file: string): Promise<string | null> {
  let cover = cache.get(file);
  if (!cover) {
    cover = api.cover(file).catch(() => null);
    cache.set(file, cover);
  }
  return cover;
}
