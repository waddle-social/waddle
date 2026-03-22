import { getAuth } from "./auth";

export async function proxyOpenIdConfiguration(
  request: Request,
  pathname = "/api/auth/.well-known/openid-configuration",
) {
  const auth = await getAuth();
  const url = new URL(request.url);
  url.pathname = pathname;
  return auth.handler(new Request(url, request));
}
