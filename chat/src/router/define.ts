// Shared codec contract for the typed route registry. Each route module
// declares its match shape and its tryParse/match/href surface directly;
// this interface only describes the smallest reusable codec primitive.

export interface SearchCodec<T> {
  // Returns either "" or a leading "?key=value&..." string.
  encode(value: T): string;
  // Accepts either a leading "?" or a bare query string.
  decode(searchString: string): T;
}
