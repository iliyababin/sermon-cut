import type { ReactNode } from "react";
import { AlertCircle } from "lucide-react";

/** The one way an error is shown: an alert with the message (already made
 * friendly by errorMessage) and optional actions. */
export function ErrorBanner({
  message,
  onRetry,
  retryLabel = "Try again",
  onReveal,
  onDismiss,
  children,
  className = "",
}: {
  message: string;
  onRetry?: () => void;
  retryLabel?: string;
  /** Show leftovers of an unfinished save (see isStuckCommit). */
  onReveal?: () => void;
  onDismiss?: () => void;
  /** More action buttons. */
  children?: ReactNode;
  className?: string;
}) {
  return (
    <div role="alert" className={`p-3 bg-destructive/10 text-destructive rounded-lg text-sm flex items-center gap-2 ${className}`}>
      <AlertCircle className="w-4 h-4 shrink-0" />
      <span className="flex-1">{message}</span>
      {onRetry && (
        <button onClick={onRetry} className="px-3 py-1 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors">
          {retryLabel}
        </button>
      )}
      {onReveal && (
        <button onClick={onReveal} className="px-3 py-1 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors">
          Show leftovers
        </button>
      )}
      {children}
      {onDismiss && (
        <button onClick={onDismiss} className="px-3 py-1 hover:bg-destructive/10 rounded-lg transition-colors">
          Dismiss
        </button>
      )}
    </div>
  );
}
