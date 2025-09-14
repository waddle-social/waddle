import { WorkOS } from '@workos-inc/node';

export function createWorkOSClient(apiKey: string): WorkOS {
  return new WorkOS(apiKey);
}

export interface WorkOSConfig {
  apiKey: string;
  clientId: string;
}

export class AuthClient {
  private workos: WorkOS;
  
  constructor(private config: WorkOSConfig) {
    this.workos = createWorkOSClient(config.apiKey);
  }
  
  getAuthorizationUrl(params: {
    redirectUri: string;
    state?: string;
    provider?: string;
  }): string {
    return this.workos.userManagement.getAuthorizationUrl({
      provider: params.provider || 'authkit',
      clientId: this.config.clientId,
      redirectUri: params.redirectUri,
      state: params.state,
    });
  }
  
  async authenticateWithCode(code: string) {
    return this.workos.userManagement.authenticateWithCode({
      code,
      clientId: this.config.clientId,
    });
  }
}