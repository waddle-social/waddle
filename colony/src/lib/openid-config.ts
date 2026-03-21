import { getAuth } from "./auth";

async function stripRegistrationEndpoint(response: Response) {
  const metadata = (await response.clone().json()) as Record<string, unknown>;
  delete metadata.registration_endpoint;

  return Response.json(metadata, {
    headers: response.headers,
    status: response.status,
    statusText: response.statusText,
  });
}

export async function proxyOpenIdConfiguration(
  request: Request,
  pathname = "/api/auth/.well-known/openid-configuration",
) {
  const auth = await getAuth();
  const url = new URL(request.url);
  url.pathname = pathname;
  const response = await auth.handler(new Request(url, request));

  return stripRegistrationEndpoint(response);
}
