// The page's server state, read and changed through TanStack Query: every read is a query (polled
// where the page follows something live), every change a mutation that invalidates what it moves.

import { QueryClient, skipToken, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ApiError,
  cancelRefund,
  createDepositAddress,
  createQuote,
  createRefund,
  getAccount,
  getAssets,
  getDepositAddress,
  getSweeps,
  getTimeline,
  getTrust,
  markRefundPaid,
  type Selection,
} from "./api.js";

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // The product's refusals (4xx) are answers, not outages: only other failures are retried.
      retry: (failures, error) => !(error instanceof ApiError && error.status < 500) && failures < 2,
    },
    mutations: { retry: false },
  },
});

export const keys = {
  account: ["account"] as const,
  assets: ["assets"] as const,
  trust: ["trust"] as const,
  depositAddress: ["deposit-address"] as const,
  sweeps: ["sweeps"] as const,
  timelines: ["timeline"] as const,
  timeline: (selection: Selection | null) => ["timeline", selection?.kind, selection?.id] as const,
};

/** The visitor's demo account: balance, ledger lines, and payments. */
export function useAccount() {
  return useQuery({ queryKey: keys.account, queryFn: getAccount, refetchInterval: 4000 });
}

/** The tokens a customer can pay with; the service's config changes rarely. */
export function useAssets() {
  return useQuery({ queryKey: keys.assets, queryFn: getAssets, staleTime: 5 * 60_000 });
}

/** The service's attestation, checked by the product (cached there for 5 minutes). */
export function useTrust() {
  return useQuery({ queryKey: keys.trust, queryFn: getTrust, staleTime: 5 * 60_000 });
}

/** The followed payment's timeline, live. */
export function useTimeline(selection: Selection | null) {
  return useQuery({
    queryKey: keys.timeline(selection),
    queryFn: selection === null ? skipToken : () => getTimeline(selection),
    refetchInterval: 3000,
  });
}

/** The visitor's deposit address and its payments, followed once the product has shown it. */
export function useDepositAddress(enabled: boolean) {
  return useQuery({ queryKey: keys.depositAddress, queryFn: getDepositAddress, enabled, refetchInterval: 3000 });
}

export function useSweeps() {
  return useQuery({ queryKey: keys.sweeps, queryFn: getSweeps, refetchInterval: 10_000 });
}

export function useCreateQuote() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: createQuote,
    onSuccess: () => client.invalidateQueries({ queryKey: keys.account }),
  });
}

export function useCreateDepositAddress() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: createDepositAddress,
    onSuccess: (created) => {
      client.setQueryData(keys.depositAddress, created);
      return client.invalidateQueries({ queryKey: keys.account });
    },
  });
}

/** A refund's change moves its timeline, and the balance once the service settles it. */
function useRefundMutation<T>(mutationFn: (variables: T) => Promise<unknown>) {
  const client = useQueryClient();
  return useMutation({
    mutationFn,
    onSuccess: () =>
      Promise.all([
        client.invalidateQueries({ queryKey: keys.timelines }),
        client.invalidateQueries({ queryKey: keys.account }),
      ]),
  });
}

export function useCreateRefund() {
  return useRefundMutation(createRefund);
}

export function useMarkRefundPaid() {
  return useRefundMutation(markRefundPaid);
}

export function useCancelRefund() {
  return useRefundMutation(cancelRefund);
}
