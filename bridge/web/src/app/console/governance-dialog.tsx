// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useEffect, useRef, type ReactNode } from "react";

export function GovernanceDialog({
  title,
  pending,
  onClose,
  children,
}: {
  title: string;
  pending: boolean;
  onClose: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    const trigger = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const overflow = document.body.style.overflow;
    dialog.showModal();
    document.body.style.overflow = "hidden";
    return () => {
      dialog.close();
      document.body.style.overflow = overflow;
      trigger?.focus();
    };
  }, []);

  return (
    <dialog
      ref={ref}
      aria-label={title}
      aria-busy={pending}
      onCancel={(event) => {
        event.preventDefault();
        if (!pending) onClose();
      }}
      className="fixed inset-0 m-auto max-h-[90dvh] w-[calc(100%-2rem)] max-w-3xl overflow-auto overscroll-contain rounded-xl border border-border bg-surface p-0 text-left text-foreground shadow-xl backdrop:bg-foreground/40"
    >
      {children}
    </dialog>
  );
}
