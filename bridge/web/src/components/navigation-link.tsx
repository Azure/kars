// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import Link, { useLinkStatus } from "next/link";
import { useLayoutEffect, type ComponentProps } from "react";

// Refreshing the old route can invalidate an unfinished navigation.
const pendingNavigations = new Set<symbol>();

export function hasPendingNavigation() {
  return pendingNavigations.size > 0;
}

export function usePendingNavigation(pending: boolean) {
  useLayoutEffect(() => {
    if (!pending) return;
    const navigation = Symbol();
    pendingNavigations.add(navigation);
    return () => {
      pendingNavigations.delete(navigation);
    };
  }, [pending]);
}

function NavigationStatus() {
  const { pending } = useLinkStatus();
  usePendingNavigation(pending);
  return pending ? (
    <span data-navigation-pending="" role="status" className="sr-only">
      Opening page...
    </span>
  ) : null;
}

export default function NavigationLink({
  children,
  className,
  ...props
}: ComponentProps<typeof Link>) {
  return (
    <Link
      {...props}
      className={`${className ?? ""} has-[[data-navigation-pending]]:cursor-progress has-[[data-navigation-pending]]:opacity-60`}
    >
      {children}
      <NavigationStatus />
    </Link>
  );
}
