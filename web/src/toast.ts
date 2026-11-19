// Всплывающие сообщения («Сохранено», «Заявка решена»): общий список вне React, показывает его <Toaster /> в оболочке.
// Мутация с `meta: { toast: t("…") }` сообщает об успехе сама (MutationCache в main.tsx).
import { useSyncExternalStore } from "react";

export type ToastTone = "ok" | "bad" | "info";
export type ToastItem = { id: number; text: string; tone: ToastTone };

declare module "@tanstack/react-query" {
  interface Register {
    mutationMeta: { toast?: string };
  }
}

let items: ToastItem[] = [];
let seq = 0;
let paused = false;
const timers = new Map<number, ReturnType<typeof setTimeout>>();
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

const lifetime = (x: ToastItem) => (x.tone === "bad" ? 8000 : 4500);

function arm(x: ToastItem) {
  clearTimeout(timers.get(x.id));
  timers.set(
    x.id,
    setTimeout(() => dismissToast(x.id), lifetime(x)),
  );
}

/** Показать сообщение; на экране не больше четырёх, ошибки держатся дольше. */
export function toast(text: string, tone: ToastTone = "ok") {
  const x = { id: ++seq, text, tone };
  const dropped = items.slice(0, Math.max(0, items.length - 3));
  for (const d of dropped) clearTimeout(timers.get(d.id));
  items = [...items.slice(-3), x];
  if (!paused) arm(x);
  emit();
}

export function dismissToast(id: number) {
  clearTimeout(timers.get(id));
  timers.delete(id);
  if (!items.some((x) => x.id === id)) return;
  items = items.filter((x) => x.id !== id);
  emit();
}

/** Указатель или фокус на сообщениях — они не исчезают; ушёл — отсчёт заново. */
export function pauseToasts(on: boolean) {
  if (paused === on) return;
  paused = on;
  for (const x of items) {
    if (on) clearTimeout(timers.get(x.id));
    else arm(x);
  }
}

function subscribe(l: () => void) {
  listeners.add(l);
  return () => listeners.delete(l);
}

export function useToasts(): ToastItem[] {
  return useSyncExternalStore(subscribe, () => items);
}
