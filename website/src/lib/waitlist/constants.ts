export const WAITLIST_QUERY_KEY = "waitlist";
export const WAITLIST_SUCCESS_MESSAGE =
	"If that address can receive Waddle waitlist mail, you’ll get a link shortly.";
export const WAITLIST_PENDING_MESSAGE = "Packing your confirmation note…";
export const WAITLIST_INVALID_MESSAGE = "Add a valid email address.";
export const WAITLIST_RETRY_MESSAGE = "Mail hiccup on our side. Try again in a minute.";
export const WAITLIST_VERIFICATION_MESSAGE = "Complete the quick human check, then try again.";
export const WAITLIST_UNAVAILABLE_MESSAGE =
	"Waitlist verification is offline right now. Try again in a minute.";
export const WAITLIST_TURNSTILE_ACTION = "waitlist_signup";

export const WAITLIST_SENDER = {
	name: "Waddle",
	email: "humans@waddle.social",
} as const;
