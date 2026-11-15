import type { RefusedKind } from "../api/client";

/** The texts shared with @grafeo-db/ui, so the move to the package changes no wording. */
export const REFUSED_TEXT: Record<RefusedKind, string> = {
  unauthenticated: "Not signed in. The server refused this request; sign in to load it.",
  forbidden: "No access. The server refused this request for this account.",
};

/**
 * What a view shows in place of its data when the server refused the request
 * (#90): never zeros or an empty list, which would read as "no data".
 */
export default function RefusedNote({
  kind,
  className,
}: {
  kind: RefusedKind;
  className?: string;
}) {
  return (
    <p role="status" className={className}>
      {REFUSED_TEXT[kind]}
    </p>
  );
}
