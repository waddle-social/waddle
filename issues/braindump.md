Braindump

* We should decide based on [https://workos.com/blog/what-is-the-difference-between-radix-and-shadcn-ui](https://workos.com/blog/what-is-the-difference-between-radix-and-shadcn-ui) whether we create our own little UI lib (similar to shadcn) or we build on shadcn  
* API design is key (we can change everything in front or behind the API as we please)  
  * I think the RKA approach could work here as well  
* We should start with a Cloudflare exclusive solution (as soon as we have more than 2-3 waddles, we should consider moving to a more “expandable” solution e.g. Rust Backend hosted inside our own k8s \- cluster)  
* We should establish strict / clear instructions for Claude Code to create (hopefully) super consistent code (an experts folder \- similar to the Personas David is using)  
* I always want to have a clean UI  
* EU based as much as possible (at least the data should live there)  
  * R2 buckets in Europe  
  * D1 location hint somewhere in Europe  
* Order of business (just an idea)  
  * Harden Authentication (it’s already there, but is lacking stuff like JWKS checks)  
    * We’re currently invite-only (no creation is possible)  
  * Implement profile CRUD  
  * Add MFA  
  * Enhance existing deployment  
  * Implement waddle creation (use Cloudflare APIs to create a D1 when creating a waddle)  
  * Build component lib  
  * Implement text chat (starting with generic rooms)  
  * Implement user settings  
  * Friend list? (Discord uses this feat for group chats)  
  * Implement group chats  
  * Android App that has all the features  
    * We start with Android so we don’t have to pay any fees to apple