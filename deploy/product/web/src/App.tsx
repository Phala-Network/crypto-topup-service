import type { Appearance } from "@phala/pay/react";
import { useCallback, useEffect, useRef, useState } from "react";
import { TooltipProvider } from "@/components/ui/tooltip";
import {
  getAccount,
  getDepositAddress,
  getTimeline,
  getTrust,
  type Account,
  type DepositAddressResponse,
  type Selection,
  type Timeline,
  type Trust,
} from "./api.js";
import { Backend } from "./Backend.js";
import { describe, usePolling } from "./common.js";
import { Product, type Method, type Session } from "./Product.js";
import { CONTAINER, Hero, Properties, SiteFooter, SiteHeader } from "./Site.js";
import { useTheme } from "./theme.js";

export function App() {
  const [theme, setTheme] = useTheme();
  const [account, setAccount] = useState<Account | null>(null);
  const [accountError, setAccountError] = useState<string | null>(null);
  const [method, setMethod] = useState<Method>("quote");
  const [session, setSession] = useState<Session | null>(null);
  const [selected, setSelected] = useState<Selection | null>(null);
  const [timeline, setTimeline] = useState<{ key: string; view: Timeline } | null>(null);
  const [trust, setTrust] = useState<Trust | null>(null);
  // The deposit address as created (with the client secret the product's UI needs), then as the
  // product last read it (its payments, for the backend).
  const [address, setAddress] = useState<{ created: DepositAddressResponse; current: DepositAddressResponse } | null>(
    null,
  );

  const refreshAccount = useCallback(() => {
    getAccount().then(
      (next) => {
        setAccount(next);
        setAccountError(null);
      },
      (error: unknown) => setAccountError(describe(error)),
    );
  }, []);
  usePolling(refreshAccount, 4000);

  useEffect(() => {
    getTrust().then(setTrust, () => setTrust(null));
  }, []);

  const selectedKey = selected === null ? null : `${selected.kind}:${selected.id}`;
  const refreshTimeline = useCallback(() => {
    if (selected === null) {
      return;
    }
    const key = `${selected.kind}:${selected.id}`;
    getTimeline(selected).then(
      (view) => setTimeline({ key, view }),
      () => undefined,
    );
  }, [selected]);
  usePolling(refreshTimeline, 3000, selected !== null);
  const refreshAll = () => {
    refreshTimeline();
    refreshAccount();
  };

  const select = useCallback((next: Selection) => {
    setSelected(next);
    setTimeline(null);
  }, []);

  // A new payment to the deposit address is followed as it arrives, as a quote is once created.
  const seenPayments = useRef<Set<string> | null>(null);
  const refreshAddress = useCallback(() => {
    getDepositAddress().then(
      (current) => {
        setAddress((previous) => (previous === null ? null : { ...previous, current }));
        const payments = current.deposit_address.payments.map((payment) => payment.deposit);
        const seen = seenPayments.current;
        const arrived = seen === null ? undefined : payments.find((deposit) => !seen.has(deposit));
        seenPayments.current = new Set(payments);
        if (arrived !== undefined) {
          select({ kind: "deposit", id: arrived });
        }
      },
      () => undefined,
    );
  }, [select]);
  usePolling(refreshAddress, 3000, address !== null);

  // The SDK's components take the page's theme tokens (src/index.css); their primary action is
  // the page's accent.
  const appearance: Appearance = {
    theme,
    variables: {
      colorPrimary: "var(--brand)",
      accessibleColorOnColorPrimary: "var(--brand-foreground)",
      colorBackground: "var(--card)",
      colorText: "var(--card-foreground)",
      colorTextSecondary: "var(--muted-foreground)",
      colorBorder: "var(--border)",
      colorDanger: "var(--destructive)",
      colorSuccess: "var(--success)",
      borderRadius: "var(--radius)",
      fontFamily: "inherit",
    },
  };

  // One page: the headline, then the product (what the customer sees) beside its backend (what
  // the merchant's server sees), then the key properties.
  return (
    <TooltipProvider delayDuration={150}>
      <div className="flex min-h-svh flex-col">
        <SiteHeader theme={theme} onThemeChange={setTheme} />
        <main id="top" className="flex-1">
          <Hero account={account} />
          <section aria-label="Live demo" className={`${CONTAINER} pb-16 lg:pb-24`}>
            <div className="grid items-start gap-x-8 gap-y-12 lg:grid-cols-[minmax(0,26rem)_minmax(0,1fr)] xl:grid-cols-[minmax(0,28rem)_minmax(0,1fr)] 2xl:gap-x-10">
              <Product
                account={account}
                accountError={accountError}
                method={method}
                onMethodChange={setMethod}
                session={session}
                onQuote={(created) => {
                  setSession({
                    quote: created.quote,
                    clientSecret: created.client_secret,
                    expectedAddress: created.expected_address,
                  });
                  select({ kind: "quote", id: created.quote });
                }}
                onNewTopUp={() => setSession(null)}
                onCredited={refreshAccount}
                address={address?.created ?? null}
                onAddress={(created) => setAddress({ created, current: created })}
                appearance={appearance}
              />
              <Backend
                account={account}
                selected={selected}
                timeline={timeline !== null && timeline.key === selectedKey ? timeline.view : null}
                trust={trust}
                address={address?.current ?? null}
                onSelect={select}
                onChanged={refreshAll}
              />
            </div>
          </section>
          <Properties />
        </main>
        <SiteFooter />
      </div>
    </TooltipProvider>
  );
}
