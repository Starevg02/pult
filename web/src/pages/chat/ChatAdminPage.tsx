// Коммуникации для суперадминистратора: групповые чаты (без сообщений), назначение владельца
// (в том числе себя — это в аудите) и обезличенная статистика по порталу. Личных диалогов здесь нет.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type AdminChat, type ChatPerson, type ChatStats } from "../../api/client";
import { formatDate } from "../../format";
import { useMe } from "../../session";
import { Badge, Button, Card, Dialog, Empty, ErrorBox, Field, PageHeader, Select, Spinner } from "../../ui";
import { UserPicker } from "./ChatPages";

function OwnerDialog({ chat, onClose }: { chat: AdminChat; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const me = useMe().data;
  const [person, setPerson] = useState<ChatPerson | null>(null);
  const [previous, setPrevious] = useState("admin");
  const save = useMutation({
    mutationFn: (user_id: string) => ok(api.POST("/api/admin/chats/{id}/owner", { params: { path: { id: chat.id } }, body: { user_id, previous } })),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["admin-chats"] });
      void qc.invalidateQueries({ queryKey: ["chats"] });
      onClose();
    },
  });
  return (
    <Dialog open onClose={onClose} title={`${t("chatAdmin.assign")}: ${chat.title}`}>
      <div className="grid gap-3">
        <Field group label={t("chatAdmin.newOwner")}>
          {person ? (
            <div className="flex items-center gap-2 text-sm">
              <span className="flex-1">{person.name}</span>
              <Button variant="ghost" onClick={() => setPerson(null)}>
                {t("common.change")}
              </Button>
            </div>
          ) : (
            <UserPicker onPick={setPerson} />
          )}
        </Field>
        {chat.owner_name && (
          <Field label={t("chatAdmin.previous", { name: chat.owner_name })}>
            <Select value={previous} onChange={(e) => setPrevious(e.target.value)}>
              <option value="admin">{t("chatAdmin.prev.admin")}</option>
              <option value="member">{t("chatAdmin.prev.member")}</option>
              <option value="remove">{t("chatAdmin.prev.remove")}</option>
            </Select>
          </Field>
        )}
        <p className="text-xs text-muted">{t("chatAdmin.assignHint")}</p>
        {save.error && <ErrorBox error={save.error} />}
        <div className="flex flex-wrap justify-end gap-2">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          {me && (
            <Button disabled={save.isPending} onClick={() => save.mutate(me.user_id)}>
              {t("chatAdmin.self")}
            </Button>
          )}
          <Button variant="primary" disabled={!person || save.isPending} onClick={() => person && save.mutate(person.id)}>
            {t("chatAdmin.assign")}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

function Stats() {
  const { t } = useTranslation();
  const q = useQuery({ queryKey: ["admin-chat-stats"], queryFn: () => ok<ChatStats>(api.GET("/api/admin/chats/stats")) });
  if (q.error) return <ErrorBox error={q.error} />;
  if (q.isPending) return <Spinner />;
  const n = (v: number | null | undefined) => (v == null ? "<5" : v.toLocaleString());
  return (
    <Card className="p-4">
      <h2 className="font-semibold">{t("chatAdmin.stats")}</h2>
      <p className="mt-1 text-xs text-muted">{t("chatAdmin.statsHint")}</p>
      <p className="mt-2 text-sm">
        {t("chatAdmin.directChats")}: {n(q.data.direct_chats)} · {t("chatAdmin.groupChats")}: {n(q.data.group_chats)}
      </p>
      {q.data.months.length > 0 && (
        <div className="mt-3 overflow-x-auto">
          <table className="w-full text-sm tabular-nums">
            <thead className="text-left text-xs text-muted">
              <tr className="border-b border-line">
                <th className="py-1.5 pr-3">{t("chatAdmin.month")}</th>
                <th className="py-1.5 pr-3">{t("chatAdmin.messages")}</th>
                <th className="py-1.5 pr-3">{t("chatAdmin.directMessages")}</th>
                <th className="py-1.5 pr-3">{t("chatAdmin.groupMessages")}</th>
                <th className="py-1.5 pr-3">{t("chatAdmin.edited")}</th>
                <th className="py-1.5">{t("chatAdmin.senders")}</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {q.data.months.map((m) => (
                <tr key={m.month}>
                  <td className="py-1.5 pr-3">{m.month}</td>
                  <td className="py-1.5 pr-3">{n(m.messages)}</td>
                  <td className="py-1.5 pr-3">{n(m.direct_messages)}</td>
                  <td className="py-1.5 pr-3">{n(m.group_messages)}</td>
                  <td className="py-1.5 pr-3">{n(m.edited)}</td>
                  <td className="py-1.5">{n(m.senders)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  );
}

export function ChatAdminPage() {
  const { t, i18n } = useTranslation();
  const [assign, setAssign] = useState<AdminChat | null>(null);
  const q = useQuery({ queryKey: ["admin-chats"], queryFn: () => ok<AdminChat[]>(api.GET("/api/admin/chats")) });
  return (
    <div className="mx-auto max-w-4xl space-y-4">
      <PageHeader title={t("admin.menu.chats")} subtitle={t("chatAdmin.hint")} />
      <Card>
        {q.error ? (
          <ErrorBox error={q.error} onRetry={() => void q.refetch()} />
        ) : q.isPending ? (
          <Spinner />
        ) : q.data.length === 0 ? (
          <Empty>{t("chatAdmin.empty")}</Empty>
        ) : (
          <ul className="divide-y divide-line">
            {q.data.map((c) => (
              <li key={c.id} className="flex flex-wrap items-center gap-2 px-4 py-2 text-sm">
                <div className="min-w-0 flex-1">
                  <div className="font-medium">{c.title}</div>
                  <div className="text-xs text-muted">
                    {t("chatAdmin.owner")}: {c.owner_name ?? "—"} · {t("chat.membersN", { n: c.members })} · {formatDate(c.created_at, i18n.language)}
                  </div>
                </div>
                {c.locked && <Badge tone="bad">{t("chatAdmin.locked")}</Badge>}
                <Button variant={c.locked ? "primary" : "ghost"} onClick={() => setAssign(c)}>
                  {t("chatAdmin.assign")}
                </Button>
              </li>
            ))}
          </ul>
        )}
      </Card>
      <Stats />
      {assign && <OwnerDialog chat={assign} onClose={() => setAssign(null)} />}
    </div>
  );
}
