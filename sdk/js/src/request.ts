/** Compose caller cancellation with a finite deadline, including response-body reads. */
export function requestSignal(options: {
  signal?: AbortSignal;
  requestTimeout?: number;
}): AbortSignal {
  const deadline = AbortSignal.timeout(options.requestTimeout ?? 10_000);
  return options.signal === undefined ? deadline : AbortSignal.any([options.signal, deadline]);
}
