// Коммуникации для суперадминистратора: групповые чаты (без сообщений), назначение владельца
// (в том числе себя — это в аудите) и обезличенная статистика по порталу. Личных диалогов здесь нет.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type AdminChat, type ChatPerson, type ChatSettings, type ChatStats, type ChatStorage } from "../../api/client";
import { formatDate } from "../../format";
import { useMe } from "../../session";
import { toast } from "../../toast";
import { Badge, Button, Card, Dialog, Empty, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";
import { formatSize } from "../helpdesk/helpdeskUtil";
import { UserPicker } from "./ChatDialogs";

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

/** Сколько места занимают переписки и сколько хранить вложения. Сообщения не удаляются никогда. */
/** Настройки «Сообщений» целиком: сохранение отправляет все поля, иначе пропущенные сбросятся. */
function useChatSettings() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["admin-chat-settings"], queryFn: () => ok<ChatSettings>(api.GET("/api/admin/chats/settings")) });
  const save = useMutation({
    mutationFn: (patch: Partial<ChatSettings>) => {
      if (!q.data) throw new Error("settings not loaded");
      return ok(api.PUT("/api/admin/chats/settings", { body: { ...q.data, ...patch } }));
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["admin-chat-settings"] });
      void qc.invalidateQueries({ queryKey: ["admin-chat-storage"] });
      void qc.invalidateQueries({ queryKey: ["chat-klipy"] });
    },
  });
  return { q, save };
}

function Storage() {
  const { t } = useTranslation();
  const q = useQuery({ queryKey: ["admin-chat-storage"], queryFn: () => ok<ChatStorage>(api.GET("/api/admin/chats/storage")) });
  const { q: cfg, save } = useChatSettings();
  const [days, setDays] = useState<string | null>(null);
  const error = q.error ?? cfg.error;
  if (error) return <ErrorBox error={error} />;
  if (q.isPending || cfg.isPending || !q.data) return <Spinner />;
  const s = q.data;
  const value = days ?? String(s.file_retention_days);
  const parsed = Number(value);
  const valid = value.trim() !== "" && Number.isInteger(parsed) && parsed >= 0 && parsed <= 3650;
  const tile = (label: string, main: string, sub?: string) => (
    <div className="rounded-xl bg-sunken px-3 py-2.5">
      <div className="text-xs text-muted">{label}</div>
      <div className="text-lg font-semibold tabular-nums">{main}</div>
      {sub && <div className="text-xs text-muted tabular-nums">{sub}</div>}
    </div>
  );
  return (
    <Card className="p-4">
      <h2 className="font-semibold">{t("chatAdmin.storage")}</h2>
      <p className="mt-1 text-xs text-muted">{t("chatAdmin.storageHint")}</p>
      <div className="mt-3 grid gap-2 sm:grid-cols-4">
        {tile(t("chatAdmin.messages"), s.messages.toLocaleString(), formatSize(s.message_bytes))}
        {tile(t("chatAdmin.files"), s.files.toLocaleString(), formatSize(s.file_bytes))}
        {tile(t("chatAdmin.chatTables"), formatSize(s.chat_tables_bytes))}
        {tile(t("chatAdmin.database"), formatSize(s.database_bytes), s.expired_files > 0 ? t("chatAdmin.expiredFiles", { count: s.expired_files }) : undefined)}
      </div>
      <form
        className="mt-4 flex flex-wrap items-end gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          if (valid)
            save.mutate(
              { file_retention_days: parsed },
              {
                onSuccess: () => {
                  setDays(null);
                  toast(t("common.saved"));
                },
              },
            );
        }}
      >
        <div className="w-56">
          <Field label={t("chatAdmin.retention")}>
            <Input type="number" min={0} max={3650} value={value} onChange={(e) => setDays(e.target.value)} />
          </Field>
        </div>
        <Button type="submit" variant="primary" disabled={!valid || days === null || save.isPending}>
          {t("common.save")}
        </Button>
      </form>
      <p className="mt-1 text-xs text-muted">{t("chatAdmin.retentionHint")}</p>
      {save.error && <ErrorBox error={save.error} />}
    </Card>
  );
}

/** Поиск GIF в KLIPY: ключ приложения из Partner Panel KLIPY; пусто — поиск выключен. */
function Klipy() {
  const { t } = useTranslation();
  const { q, save } = useChatSettings();
  const [key, setKey] = useState<string | null>(null);
  if (q.error) return <ErrorBox error={q.error} />;
  if (q.isPending) return <Spinner />;
  const current = q.data.klipy_key ?? "";
  const value = key ?? current;
  const done = (msg: string) => ({
    onSuccess: () => {
      setKey(null);
      toast(msg);
    },
  });
  return (
    <Card className="p-4">
      <div className="flex items-center gap-2">
        <h2 className="font-semibold">{t("chatAdmin.klipy")}</h2>
        <Badge tone={current ? "ok" : "muted"}>{current ? t("chatAdmin.klipyOn") : t("chatAdmin.klipyOff")}</Badge>
      </div>
      <p className="mt-1 text-xs text-muted">{t("chatAdmin.klipyHint")}</p>
      <form
        className="mt-3 flex flex-wrap items-end gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          if (key !== null && value.trim()) save.mutate({ klipy_key: value.trim() }, done(t("common.saved")));
        }}
      >
        <div className="w-full max-w-md">
          <Field label={t("chatAdmin.klipyKey")}>
            <Input value={value} onChange={(e) => setKey(e.target.value)} autoComplete="off" spellCheck={false} maxLength={128} />
          </Field>
        </div>
        <Button type="submit" variant="primary" disabled={key === null || !value.trim() || value.trim() === current || save.isPending}>
          {t("common.save")}
        </Button>
        {current && (
          <Button type="button" disabled={save.isPending} onClick={() => save.mutate({ klipy_key: "" }, done(t("chatAdmin.klipyRemoved")))}>
            {t("chatAdmin.klipyRemove")}
          </Button>
        )}
      </form>
      <p className="mt-2 text-xs text-muted">{t("chatAdmin.klipyPrivacy")}</p>
      {save.error && <ErrorBox error={save.error} />}
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
      <Storage />
      <Klipy />
      <Stats />
      {assign && <OwnerDialog chat={assign} onClose={() => setAssign(null)} />}
    </div>
  );
}
