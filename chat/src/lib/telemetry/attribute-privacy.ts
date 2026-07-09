const PRIVACY_SAFE_TRACE_RESOURCE_ATTRIBUTE_KEYS = new Set([
  "deploymentenvironment",
  "deploymentenvironmentname",
  "servicename",
  "servicenamespace",
  "serviceversion",
  "telemetrydistroname",
  "telemetrydistroversion",
  "telemetrysdklanguage",
  "telemetrysdkname",
  "telemetrysdkversion",
]);

export function isPrivacySafeTraceResourceAttributeKey(key: string): boolean {
  return PRIVACY_SAFE_TRACE_RESOURCE_ATTRIBUTE_KEYS.has(normalizeAttributeKey(key));
}

export function isForbiddenTelemetryAttributeKey(key: string): boolean {
  const normalized = normalizeAttributeKey(key);
  return isIdentifierAttributeKey(key)
    || normalized.includes("useragent")
    || normalized.startsWith("browser")
    || normalized.startsWith("exception")
    || normalized === "errormessage"
    || normalized === "errorstack"
    || normalized === "errorstacktrace"
    || normalized === "processruntimename"
    || normalized === "processruntimeversion"
    || normalized === "runtimename"
    || normalized === "runtimeversion"
    || normalized === "faroactionuserparentid";
}

function isIdentifierAttributeKey(key: string): boolean {
  const normalized = normalizeAttributeKey(key);
  return normalized.includes("authorization")
    || normalized.includes("accesstoken")
    || normalized.includes("refreshtoken")
    || normalized.includes("idtoken")
    || normalized.includes("sessionid")
    || normalized.includes("previoussession")
    || normalized.includes("installationid")
    || normalized.includes("accountid")
    || normalized.includes("accountkey")
    || normalized.includes("userid")
    || normalized.includes("useremail")
    || normalized.includes("username")
    || normalized.includes("userfullname")
    || normalized.includes("userhash")
    || normalized.includes("messageid")
    || normalized.includes("stanzaid")
    || normalized.includes("streamid")
    || normalized.includes("roomid")
    || normalized.includes("channelid")
    || normalized === "email"
    || normalized === "jid"
    || normalized === "barejid"
    || normalized === "fulljid"
    || normalized === "peer";
}

function normalizeAttributeKey(key: string): string {
  return key.replace(/[^a-z0-9]/gi, "").toLowerCase();
}
