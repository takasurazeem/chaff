/**
 * A rating, as stars.
 *
 * Rendered as text rather than as icons: a five-star rating is five characters, and an
 * icon font or SVG set is a dependency and a loading state for something the user reads
 * at a glance. The accessible name carries the number, because "★★★★☆" is not something
 * a screen reader should attempt to pronounce.
 */
export function Stars({ rating, className = "" }: { rating: number; className?: string }) {
  if (rating <= 0) return null;
  return (
    <span
      className={`select-none text-[10px] leading-none tracking-tight text-amber-300 ${className}`}
      role="img"
      aria-label={`${rating} of 5 stars`}
      title={`${rating} of 5 stars`}
    >
      {"★".repeat(rating)}
    </span>
  );
}
