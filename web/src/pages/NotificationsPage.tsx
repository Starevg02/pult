// Уведомления в портале: список и «прочитано».
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { api, ok, type NotificationList } from "../api/client";
import { Button, Card, Empty, ErrorBox, PageHeader, Spinner } from "../ui";
import { formatDate } from "../format";
import { dyn } from "./helpdesk/helpdeskUtil";
import { TicketLink } from "./helpdesk/parts";

// Подпись вида — из словаря `notifications.kind.*`; неизвестный вид — «Уведомление».
// Что у каждого вида сервера есть подпись, проверяет тест `crates/pult-helpdesk/tests/ui_labels.rs`.
type TicketPayload = { number?: number; title?: string; error?: unknown };
// Оповещение мониторинга: источник, название, узел.
type AlertPayload = { source?: string; title?: string; host?: string | null };
// Коммуникации: чат и его название.
type ChatPayload = { chat?: string; title?: string | null };

export function NotificationsPage() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const kindLabel = (k: string) => (i18n.exists(`notifications.kind.${k}`) ? dyn(t, `notifications.kind.${k}`) : t("notifications.kind.other"));
  const q = useQuery({ queryKey: ["notifications"], queryFn: () => ok<NotificationList>(api.GET("/api/notifications")) });
  const read = useMutation({
    mutationFn: (ids: string[]) => ok(api.POST("/api/notifications/read", { body: { ids } })),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["notifications"] }),
  });
  return (
    <div className="mx-auto max-w-3xl">
      <PageHeader title={t("notifications.title")}>
        {(q.data?.unread ?? 0) > 0 && <Button onClick={() => read.mutate([])}>{t("notifications.readAll")}</Button>}
      </PageHeader>
      {q.error ? (
        <ErrorBox error={q.error} onRetry={() => void q.refetch()} />
      ) : q.isPending ? (
        <Spinner />
      ) : q.data.items.length === 0 ? (
        <Empty>{t("notifications.empty")}</Empty>
      ) : (
        <Card>
          <ul className="divide-y divide-line">
            {q.data.items.map((n) => {
              // Переход по уведомлению — значит, его прочитали.
              const opened = n.read_at ? undefined : () => read.mutate([n.id]);
              return (
              <li key={n.id} className="flex items-start gap-3 px-4 py-3 text-sm">
                {!n.read_at && <span className="mt-1.5 h-2 w-2 shrink-0 rounded-full bg-accent" aria-label={t("notifications.unread")} />}
                <div className="min-w-0 flex-1">
                  <div className={n.read_at ? "" : "font-semibold"}>{kindLabel(n.kind)}</div>
                  {typeof (n.payload as unknown as TicketPayload).number === "number" ? (
                    <TicketLink number={(n.payload as unknown as Required<TicketPayload>).number} className="truncate text-xs text-accent" onClick={opened}>
                      #{(n.payload as unknown as TicketPayload).number} {(n.payload as unknown as TicketPayload).title}
                    </TicketLink>
                  ) : typeof (n.payload as unknown as ChatPayload).chat === "string" ? (
                    <Link to="/chats/$id" params={{ id: String((n.payload as unknown as ChatPayload).chat) }} className="block truncate text-xs text-accent" onClick={opened}>
                      {(n.payload as unknown as ChatPayload).title ?? ""}
                    </Link>
                  ) : typeof (n.payload as unknown as AlertPayload).title === "string" ? (
                    <div className="truncate text-xs text-muted">
                      {[(n.payload as unknown as AlertPayload).source, (n.payload as unknown as AlertPayload).title, (n.payload as unknown as AlertPayload).host].filter(Boolean).join(" · ")}
                    </div>
                  ) : (
                    <div className="truncate text-xs text-muted">{typeof (n.payload as unknown as TicketPayload).error === "string" ? String((n.payload as unknown as TicketPayload).error) : JSON.stringify(n.payload)}</div>
                  )}
                </div>
                <span className="shrink-0 text-xs text-muted">{formatDate(n.created_at, i18n.language)}</span>
                {!n.read_at && (
                  <Button variant="ghost" onClick={() => read.mutate([n.id])}>
                    {t("notifications.markRead")}
                  </Button>
                )}
              </li>
              );
            })}
          </ul>
        </Card>
      )}
    </div>
  );
}
