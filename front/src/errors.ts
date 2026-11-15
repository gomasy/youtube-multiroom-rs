import { UnauthorizedError } from "./api";
import { t } from "./i18n";

/**
 * Single place where a rejected API call becomes a user-visible toast.
 * Two kinds of rejection are deliberately silent:
 * - UnauthorizedError: onUnauthorized already opened the auth modal.
 * - AbortError: the caller cancelled the request, so there is nothing to report.
 */
export function showApiError(showToast: (msg: string) => void, e: unknown) {
  if (e instanceof UnauthorizedError) return;
  if (e instanceof DOMException && e.name === "AbortError") return;
  showToast(`${t("common.error")}: ${e instanceof Error ? e.message : String(e)}`);
}

/** Run `action`, toasting what it throws. */
export async function reportErrors(
  showToast: (msg: string) => void,
  action: () => Promise<void>,
): Promise<void> {
  try {
    await action();
  } catch (e) {
    showApiError(showToast, e);
  }
}

/** reportErrors, with `setBusy` held true for the duration. */
export async function withBusy(
  setBusy: (busy: boolean) => void,
  showToast: (msg: string) => void,
  action: () => Promise<void>,
): Promise<void> {
  setBusy(true);
  try {
    await reportErrors(showToast, action);
  } finally {
    setBusy(false);
  }
}
