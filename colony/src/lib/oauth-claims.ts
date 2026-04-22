export const oauthClaimsSupported = [
  "sub",
  "iss",
  "aud",
  "exp",
  "iat",
  "sid",
  "scope",
  "azp",
  "name",
  "email",
  "email_verified",
  "picture",
  "preferred_username",
] as const;

type ServerIdTokenClaimsInput = {
  metadata?: Record<string, unknown>;
  user: {
    githubUsername?: unknown;
    image?: unknown;
  };
};

export function getServerIdTokenClaims({
  metadata,
  user,
}: ServerIdTokenClaimsInput) {
  if (metadata?.product !== "server") {
    return {};
  }

  const picture = typeof user.image === "string" ? user.image.trim() : "";
  const githubUsername =
    typeof user.githubUsername === "string" ? user.githubUsername.trim() : "";
  const claims: Record<string, string> = {};

  if (githubUsername) {
    claims.preferred_username = githubUsername;
  }
  if (picture) {
    claims.picture = picture;
  }

  return claims;
}
