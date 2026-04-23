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
  metadata?: Record<string, unknown> | undefined;
  scopes?: readonly string[] | undefined;
  user: {
    githubUsername?: unknown;
    image?: unknown;
  };
};

type UserInfoClaimsInput = {
  scopes?: readonly string[] | undefined;
  user: {
    githubUsername?: unknown;
    image?: unknown;
  };
};

function shouldExposeProfileClaims(
  metadata: Record<string, unknown> | undefined,
  scopes: readonly string[] | undefined,
) {
  return metadata?.product === "server" || scopes?.includes("profile") === true;
}

function getProfileClaims({
  metadata,
  scopes,
  user,
}: ServerIdTokenClaimsInput) {
  if (!shouldExposeProfileClaims(metadata, scopes)) {
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

export function getServerIdTokenClaims(input: ServerIdTokenClaimsInput) {
  return getProfileClaims(input);
}

export function getServerUserInfoClaims({ scopes, user }: UserInfoClaimsInput) {
  return getProfileClaims({ scopes, user });
}
