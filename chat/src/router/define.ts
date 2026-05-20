// Shared codec contract for the typed route registry. Each route module
// declares its match shape and its tryParse/match/href surface directly;
// this interface only describes the smallest reusable codec primitive.

export interface SearchCodec<T> {
  // Returns either "" or a leading "?key=value&..." string.
  encode(value: T): string;
  // Accepts either a leading "?" or a bare query string.
  decode(searchString: string): T;
}

// Factory for routes with no params and no search (just a static path).
// Used by home, threads, feed, stories, events, dmList — each route
// would otherwise hand-roll the same 12-line module.
export function staticRoute<Id extends string>(id: Id, path: string) {
  type Match = { readonly id: Id };
  return {
    id,
    match(): Match {
      return { id };
    },
    href(): string {
      return path;
    },
    tryParse(pathname: string, _searchString: string): Match | null {
      return pathname === path ? { id } : null;
    },
  };
}
