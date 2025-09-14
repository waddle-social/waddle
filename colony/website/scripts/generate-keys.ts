#!/usr/bin/env bun

import { generateES256KeyPair } from '../src/lib/auth/keys';

async function main() {
  console.log('🔐 Generating ES256 key pair for Bluesky OAuth...\n');
  
  const { privateKey, publicKey } = await generateES256KeyPair();
  
  console.log('📄 Public Key (for /jwks.json endpoint):');
  console.log(JSON.stringify(publicKey, null, 2));
  console.log('\n');
  
  console.log('🔑 Private Key (store as BLUESKY_PRIVATE_KEY environment variable):');
  console.log(JSON.stringify(privateKey, null, 2));
  console.log('\n');
  
  console.log('⚠️  IMPORTANT:');
  console.log('1. Store the private key securely in your environment variables');
  console.log('2. Never commit the private key to version control');
  console.log('3. The public key will be exposed via the /jwks.json endpoint');
  console.log('4. Keep the kid (Key ID) consistent between environments');
}

main().catch(console.error);