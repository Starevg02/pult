// Данные профиля: своя карточка, присутствие коллег, сигнал открытой вкладки.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef } from "react";
import { api, ok, type MyCard, type PresenceEntry, type PresenceMode } from "../../api/client";

export const MODES: PresenceMode[] = ["auto", "away", "dnd", "invisible"];

/** Адрес картинки аватара; версия в адресе меняется вместе с картинкой. */
export const avatarUrl = (userId: string, version: string) => `/api/users/${userId}/avatar?v=${version}`;

export function useMyCard() {
  return useQuery({ queryKey: ["my-card"], queryFn: () => ok<MyCard>(api.GET("/api/me/card")), staleTime: 60_000 });
}

export function useSaveStatus() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: { presence: PresenceMode; status_emoji?: string | null; status_text?: string | null; status_until?: string | null }) =>
      ok<MyCard>(api.PUT("/api/me/status", { body })),
    onSuccess: (c) => {
      qc.setQueryData(["my-card"], c);
      void qc.invalidateQueries({ queryKey: ["presence"] });
      void qc.invalidateQueries({ queryKey: ["user-card"] });
    },
  });
}

/** Присутствие, свой статус и аватар коллег по пользователям или сотрудникам справочника. Обновляется раз в минуту. */
export function usePresence({ users = [], persons = [] }: { users?: string[]; persons?: string[] }) {
  const u = [...new Set(users)].sort().slice(0, 200);
  const p = [...new Set(persons)].sort().slice(0, 200);
  const q = useQuery({
    queryKey: ["presence", u.join(","), p.join(",")],
    queryFn: () => ok<PresenceEntry[]>(api.GET("/api/users/presence", { params: { query: { users: u.join(",") || undefined, persons: p.join(",") || undefined } } })),
    enabled: u.length + p.length > 0,
    refetchInterval: 60_000,
    staleTime: 30_000,
  });
  const byUser = new Map<string, PresenceEntry>();
  const byPerson = new Map<string, PresenceEntry>();
  for (const e of q.data ?? []) {
    byUser.set(e.user_id, e);
    if (e.person_id) byPerson.set(e.person_id, e);
  }
  return { byUser, byPerson };
}

/** Без действий столько — вкладка сообщает «нет на месте». */
const IDLE_MS = 10 * 60_000;
const BEAT_MS = 60_000;

/** Сигнал открытой вкладки: раз в минуту и сразу при смене «нет действий». */
export function useHeartbeat() {
  const last = useRef(0);
  const idle = useRef(false);
  useEffect(() => {
    last.current = Date.now();
    const send = () => {
      void api.POST("/api/me/heartbeat", { body: { idle: idle.current } }).catch(() => undefined);
    };
    const check = () => {
      const now = Date.now() - last.current > IDLE_MS;
      if (now !== idle.current) {
        idle.current = now;
        send();
      }
    };
    const active = () => {
      last.current = Date.now();
      check();
    };
    const shown = () => {
      if (document.visibilityState === "visible") active();
    };
    const events = ["pointerdown", "keydown", "wheel", "touchstart"] as const;
    for (const e of events) window.addEventListener(e, active, { passive: true });
    document.addEventListener("visibilitychange", shown);
    send();
    const beat = window.setInterval(() => {
      check();
      send();
    }, BEAT_MS);
    const idleCheck = window.setInterval(check, 30_000);
    return () => {
      for (const e of events) window.removeEventListener(e, active);
      document.removeEventListener("visibilitychange", shown);
      window.clearInterval(beat);
      window.clearInterval(idleCheck);
    };
  }, []);
}
