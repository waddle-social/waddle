import type {
  ActivityPublication,
  GeneralActivity,
  MoodKind,
  MoodPublication,
  TunePublication,
} from "@/lib/xmpp/pep-types";
import {
  GENERAL_ACTIVITIES,
  MOOD_KINDS,
} from "@/lib/xmpp/pep-types";

export { GENERAL_ACTIVITIES, MOOD_KINDS };

export interface MoodStatusDraft {
  kind: MoodKind | "";
  text: string;
}

export interface ActivityStatusDraft {
  general: GeneralActivity | "";
  specific: string;
  text: string;
}

export interface TuneStatusDraft {
  artist: string;
  title: string;
  source: string;
  length: string;
  rating: string;
  track: string;
  uri: string;
}

export interface ActivityValidationErrors {
  general?: string;
  specific?: string;
}

export interface TuneValidationErrors {
  form?: string;
  length?: string;
  rating?: string;
  uri?: string;
}

const SNAKE_CASE_PATTERN = /^[a-z0-9]+(?:_[a-z0-9]+)*$/;

function trimOptional(value: string): string | undefined {
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

export function formatPepKeyword(value: string): string {
  return value
    .split("_")
    .map((segment) => segment.charAt(0).toUpperCase() + segment.slice(1))
    .join(" ");
}

export function normalizeActivitySpecific(value: string): string {
  return value
    .trim()
    .toLowerCase()
    .replace(/['’]/g, "")
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
}

export function buildMoodPublication(draft: MoodStatusDraft): {
  publication: MoodPublication | null;
  error?: string;
} {
  if (!draft.kind) {
    return {
      publication: null,
      error: "Choose a mood to share.",
    };
  }

  return {
    publication: {
      kind: draft.kind,
      text: trimOptional(draft.text),
    },
  };
}

export function buildActivityPublication(draft: ActivityStatusDraft): {
  publication: ActivityPublication | null;
  errors: ActivityValidationErrors;
} {
  const errors: ActivityValidationErrors = {};

  if (!draft.general) {
    errors.general = "Pick a broad activity first.";
  }

  const specific = normalizeActivitySpecific(draft.specific);
  if (draft.specific.trim().length > 0 && specific.length === 0) {
    errors.specific = "Use letters or numbers for the specific activity.";
  } else if (specific.length > 0 && !SNAKE_CASE_PATTERN.test(specific)) {
    errors.specific = "Specific activity must stay in snake_case.";
  }

  if (errors.general || errors.specific) {
    return { publication: null, errors };
  }

  return {
    publication: {
      general: draft.general as GeneralActivity,
      specific: specific || undefined,
      text: trimOptional(draft.text),
    },
    errors,
  };
}

function isAbsoluteUri(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol.length > 1;
  } catch {
    return false;
  }
}

export function buildTunePublication(draft: TuneStatusDraft): {
  publication: TunePublication | null;
  errors: TuneValidationErrors;
} {
  const errors: TuneValidationErrors = {};
  const publication: TunePublication = {
    artist: trimOptional(draft.artist),
    title: trimOptional(draft.title),
    source: trimOptional(draft.source),
    track: trimOptional(draft.track),
  };

  const lengthValue = draft.length.trim();
  if (lengthValue.length > 0) {
    if (!/^\d+$/.test(lengthValue)) {
      errors.length = "Length must be whole seconds.";
    } else {
      const parsedLength = Number.parseInt(lengthValue, 10);
      if (parsedLength < 1) {
        errors.length = "Length must be at least 1 second.";
      } else {
        publication.length = parsedLength;
      }
    }
  }

  const ratingValue = draft.rating.trim();
  if (ratingValue.length > 0) {
    if (!/^\d+$/.test(ratingValue)) {
      errors.rating = "Rating must be a whole number from 1 to 10.";
    } else {
      const parsedRating = Number.parseInt(ratingValue, 10);
      if (parsedRating < 1 || parsedRating > 10) {
        errors.rating = "Rating must stay between 1 and 10.";
      } else {
        publication.rating = parsedRating;
      }
    }
  }

  const uriValue = draft.uri.trim();
  if (uriValue.length > 0) {
    if (!isAbsoluteUri(uriValue)) {
      errors.uri = "Use a full URL or URI, like https://… or spotify:track:…";
    } else {
      publication.uri = uriValue;
    }
  }

  const hasDetails = Object.values(publication).some((value) => value !== undefined);
  if (!hasDetails) {
    errors.form = "Add at least one tune detail or use Clear to remove your current tune.";
  }

  if (errors.form || errors.length || errors.rating || errors.uri) {
    return { publication: null, errors };
  }

  return { publication, errors };
}

export function describeMoodPublication(publication: MoodPublication): string {
  const label = formatPepKeyword(publication.kind);
  return publication.text ? `Shared as ${label}: “${publication.text}”` : `Shared as ${label}.`;
}

export function describeActivityPublication(publication: ActivityPublication): string {
  const base = formatPepKeyword(publication.general);
  const specific = publication.specific ? ` · ${formatPepKeyword(publication.specific)}` : "";
  const text = publication.text ? ` — ${publication.text}` : "";
  return `Shared as ${base}${specific}${text}`;
}

export function describeTunePublication(publication: TunePublication): string {
  const lead = [publication.title, publication.artist].filter(Boolean).join(" — ");
  if (lead.length > 0) return `Shared ${lead}.`;
  if (publication.source) return `Shared from ${publication.source}.`;
  if (publication.uri) return `Shared ${publication.uri}.`;
  return "Tune shared.";
}
