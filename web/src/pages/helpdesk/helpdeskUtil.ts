// Общее для экранов Helpdesk: события SSE, загрузка файлов, оформление статусов.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { TFunction } from "i18next";
import { useEffect } from "react";
import { api, ApiError, csrfToken, ok, type AnnouncementView } from "../../api/client";

/** Перевод ключа, собранного из значения с сервера (статус, канал…). */
export const dyn = (t: TFunction, key: string) => t(key as never) as string;

export function useAnnouncements() {
  return useQuery({ queryKey: ["announcements"], queryFn: () => ok<AnnouncementView[]>(api.GET("/api/helpdesk/announcements")), refetchInterval: 60_000 });
}

export type Tone = "muted" | "ok" | "bad" | "accent";

export const statusTone: Record<string, Tone> = {
  new: "bad",
  assigned: "accent",
  in_progress: "accent",
  waiting: "muted",
  waiting_approval: "muted",
  on_hold: "muted",
  resolved: "ok",
  closed: "muted",
  canceled: "muted",
};

export const priorityTone: Record<string, Tone> = { low: "muted", normal: "muted", high: "accent", critical: "bad" };

export const STATUSES = ["new", "assigned", "in_progress", "waiting", "waiting_approval", "on_hold", "resolved", "closed", "canceled"] as const;
export const PRIORITIES = ["low", "normal", "high", "critical"] as const;

export type Uploaded = { id: string; name: string; is_image: boolean };

export async function uploadFile(file: File): Promise<Uploaded> {
  const form = new FormData();
  form.append("file", file, file.name || "image.png");
  const headers: Record<string, string> = {};
  const t = csrfToken();
  if (t) headers["X-CSRF-Token"] = t;
  const r = await fetch("/api/helpdesk/files", { method: "POST", body: form, headers, credentials: "same-origin" });
  if (!r.ok) {
    const b = (await r.json().catch(() => ({}))) as { error?: string; message?: string };
    throw new ApiError(r.status, b.error ?? "upload", b.message ?? r.statusText);
  }
  return (await r.json()) as Uploaded;
}

export const fileUrl = (id: string) => `/api/helpdesk/files/${id}`;

export function formatSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** Подписка на события сервера: списки и карточки обновляются без перезагрузки. */
export function useServerEvents(enabled: boolean) {
  const qc = useQueryClient();
  useEffect(() => {
    if (!enabled) return;
    const es = new EventSource("/api/events");
    const tickets = () => {
      void qc.invalidateQueries({ queryKey: ["tickets"] });
      void qc.invalidateQueries({ queryKey: ["ticket"] });
    };
    const notifications = () => void qc.invalidateQueries({ queryKey: ["notifications"] });
    const announcements = () => void qc.invalidateQueries({ queryKey: ["announcements"] });
    es.addEventListener("ticket", tickets);
    es.addEventListener("notification", notifications);
    es.addEventListener("announcement", announcements);
    es.addEventListener("refresh", () => {
      tickets();
      notifications();
      announcements();
    });
    return () => es.close();
  }, [enabled, qc]);
}
