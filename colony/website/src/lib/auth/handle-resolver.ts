/**
 * Custom handle resolver for Cloudflare Workers environment
 * Since we can't use DNS queries directly, we'll use HTTP-based resolution
 */

export async function resolveHandle(handle: string): Promise<string> {
  // Remove @ if present
  handle = handle.replace(/^@/, '');
  
  console.log('Resolving handle:', handle);
  
  // Try .well-known resolution first (works for custom domains)
  try {
    const wellKnownUrl = `https://${handle}/.well-known/atproto-did`;
    console.log('Trying .well-known at:', wellKnownUrl);
    
    const response = await fetch(wellKnownUrl, {
      method: 'GET',
      headers: {
        'Accept': 'text/plain',
      },
      // Short timeout for well-known check
      signal: AbortSignal.timeout(5000),
    });
    
    if (response.ok) {
      const did = (await response.text()).trim();
      console.log('Resolved via .well-known:', did);
      if (did.startsWith('did:')) {
        return did;
      }
    }
  } catch (error) {
    console.log('.well-known resolution failed:', error);
  }
  
  // For .bsky.social handles, use the Bluesky API directly
  if (handle.endsWith('.bsky.social')) {
    try {
      const apiUrl = `https://bsky.social/xrpc/com.atproto.identity.resolveHandle?handle=${handle}`;
      console.log('Trying Bluesky API at:', apiUrl);
      
      const response = await fetch(apiUrl, {
        method: 'GET',
        headers: {
          'Accept': 'application/json',
        },
      });
      
      if (response.ok) {
        const data = await response.json();
        console.log('Resolved via Bluesky API:', data.did);
        return data.did;
      }
    } catch (error) {
      console.log('Bluesky API resolution failed:', error);
    }
  }
  
  // For custom domains, we can try using a public DNS-over-HTTPS service
  // to check the TXT record
  try {
    const dnsHost = "https://1.1.1.1/dns-query?";
    const dnsQuery = dnsHost + `type=TXT&name=_atproto.${handle}`;
    console.log('Trying DNS-over-HTTPS at:', dnsQuery);
    
    const response = await fetch(dnsQuery, {
      method: 'GET',
      headers: {
        'accept': 'application/dns-json',
      },
    });
    
    if (response.ok) {
      const data = await response.json();
      console.log('DNS response:', JSON.stringify(data));
      
      if (data.Answer && data.Answer.length > 0) {
        for (const answer of data.Answer) {
          if (answer.type === 16 && answer.data) { // TXT record
            // Remove quotes
            let txtValue = answer.data.replace(/"/g, '');
            console.log('TXT record value:', txtValue);
            
            // Handle both formats: "did:plc:..." and "did=did:plc:..."
            if (txtValue.startsWith('did=')) {
              txtValue = txtValue.substring(4); // Remove "did=" prefix
            }
            
            if (txtValue.startsWith('did:')) {
              console.log('Resolved via DNS-over-HTTPS:', txtValue);
              return txtValue;
            }
          }
        }
      }
    }
  } catch (error) {
    console.log('DNS-over-HTTPS resolution failed:', error);
  }
  
  throw new Error(`Failed to resolve handle: ${handle}`);
}