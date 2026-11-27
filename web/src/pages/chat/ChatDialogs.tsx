// Диалоги коммуникаций: поиск сотрудника, новая переписка, участники группы, версии сообщения, пересылка.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type ChatDetail, type ChatMessage, type ChatPerson, type ChatMemberEvent, type ChatMessageVersion } from "../../api/client";
import { UserAvatar } from "../people/presence";
import { usePresence } from "../people/profileHooks";
import { formatDate } from "../../format";
import { useMe } from "../../session";
import { toast } from "../../toast";
import { Badge, Button, Dialog, Empty, ErrorBox, Field, Input, MenuButton, Spinner } from "../../ui";
import { dyn } from "../helpdesk/helpdeskUtil";
import { useChats } from "./chatUtil";

/** Поиск сотрудника (действующие учётки). */
export function UserPicker({ onPick, exclude = [] }: { onPick: (p: ChatPerson) => void; exclude?: string[] }) {
  const { t } = useTranslation();
  const [q, setQ] = useState("");
  const found = useQuery({
    queryKey: ["chat-people", q.trim()],
    queryFn: () => ok<ChatPerson[]>(api.GET("/api/chat/people", { params: { query: { q: q.trim() } } })),
    enabled: q.trim().length >= 2,
  });
  const list = (found.data ?? []).filter((p) => !exclude.includes(p.id));
  return (
    <div>
      <Input type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("chat.searchPeople")} aria-label={t("chat.searchPeople")} />
      {q.trim().length >= 2 && (
        <ul className="mt-1 max-h-56 divide-y divide-line overflow-y-auto rounded-lg border border-line">
          {found.isPending ? (
            <li className="px-3 py-2 text-sm text-muted">…</li>
          ) : list.length === 0 ? (
            <li className="px-3 py-2 text-sm text-muted">{t("chat.nobody")}</li>
          ) : (
            list.map((p) => (
              <li key={p.id}>
                <button
                  type="button"
                  className="w-full px-3 py-2 text-left text-sm hover:bg-hover"
                  onClick={() => {
                    onPick(p);
                    setQ("");
                  }}
                >
                  {p.name}
                  {p.department && <span className="block text-xs text-muted">{p.department}</span>}
                </button>
              </li>
            ))
          )}
        </ul>
      )}
    </div>
  );
}

export function NewChatDialog({ mode, onClose }: { mode: "direct" | "group"; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const nav = useNavigate();
  const [title, setTitle] = useState("");
  const [people, setPeople] = useState<ChatPerson[]>([]);
  const opened = (id: string) => {
    void qc.invalidateQueries({ queryKey: ["chats"] });
    onClose();
    void nav({ to: "/chats/$id", params: { id } });
  };
  const direct = useMutation({
    mutationFn: (user_id: string) => ok<{ id: string }>(api.POST("/api/chat/direct", { body: { user_id } })),
    onSuccess: (r) => opened(r.id),
  });
  const group = useMutation({
    mutationFn: () => ok<{ id: string }>(api.POST("/api/chat/groups", { body: { title, members: people.map((p) => p.id) } })),
    onSuccess: (r) => opened(r.id),
  });
  const error = direct.error ?? group.error;
  return (
    <Dialog open onClose={onClose} title={mode === "direct" ? t("chat.newDirect") : t("chat.newGroup")}>
      {mode === "direct" ? (
        <UserPicker onPick={(p) => direct.mutate(p.id)} />
      ) : (
        <div className="grid gap-3">
          <Field label={t("chat.title")}>
            <Input value={title} maxLength={200} onChange={(e) => setTitle(e.target.value)} />
          </Field>
          <Field group label={t("chat.members")}>
            {people.length > 0 && (
              <div className="mb-2 flex flex-wrap gap-1.5">
                {people.map((p) => (
                  <button key={p.id} type="button" className="rounded-full bg-hover px-2.5 py-0.5 text-xs" onClick={() => setPeople(people.filter((x) => x.id !== p.id))} aria-label={`${t("common.remove")}: ${p.name}`}>
                    {p.name} ×
                  </button>
                ))}
              </div>
            )}
            <UserPicker exclude={people.map((p) => p.id)} onPick={(p) => setPeople([...people, p])} />
          </Field>
          <p className="text-xs text-muted">{t("chat.groupHint")}</p>
          <div className="flex justify-end gap-2">
            <Button onClick={onClose}>{t("common.cancel")}</Button>
            <Button variant="primary" disabled={!title.trim() || group.isPending} onClick={() => group.mutate()}>
              {t("chat.create")}
            </Button>
          </div>
        </div>
      )}
      {error && <ErrorBox error={error} />}
    </Dialog>
  );
}

export function Versions({ chat, message, onClose }: { chat: string; message: ChatMessage; onClose: () => void }) {
  const { t, i18n } = useTranslation();
  const q = useQuery({
    queryKey: ["chat-versions", message.id, message.version],
    queryFn: () => ok<ChatMessageVersion[]>(api.GET("/api/chat/{id}/messages/{message}/versions", { params: { path: { id: chat, message: message.id } } })),
  });
  return (
    <Dialog open onClose={onClose} title={t("chat.versions")}>
      {q.error ? (
        <ErrorBox error={q.error} />
      ) : q.isPending ? (
        <Spinner />
      ) : (
        <ol className="space-y-3 text-sm">
          {q.data.map((v) => (
            <li key={v.version}>
              <div className="text-xs text-muted">
                {t("chat.version", { n: v.version })} · {formatDate(v.created_at, i18n.language)}
              </div>
              <p className="whitespace-pre-wrap">{v.body}</p>
            </li>
          ))}
          <li>
            <div className="text-xs text-muted">
              {t("chat.current")} · {formatDate(message.edited_at ?? message.created_at, i18n.language)}
            </div>
            <p className="whitespace-pre-wrap">{message.body}</p>
          </li>
        </ol>
      )}
    </Dialog>
  );
}

function MemberHistory({ chat }: { chat: string }) {
  const { t, i18n } = useTranslation();
  const q = useQuery({ queryKey: ["chat-member-events", chat], queryFn: () => ok<ChatMemberEvent[]>(api.GET("/api/chat/{id}/member-events", { params: { path: { id: chat } } })) });
  if (q.error) return <ErrorBox error={q.error} />;
  if (q.isPending) return <Spinner />;
  return (
    <ul className="max-h-64 space-y-1 overflow-y-auto text-xs">
      {q.data.map((e, i) => (
        <li key={i}>
          <span className="text-muted">{formatDate(e.created_at, i18n.language)}</span> · {e.user_name} — {dyn(t, `chat.actions.${e.action}`)}
          {e.actor_name && e.actor_name !== e.user_name && <span className="text-muted"> ({e.actor_name})</span>}
        </li>
      ))}
    </ul>
  );
}

export function ChatInfo({ d, onClose }: { d: ChatDetail; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const nav = useNavigate();
  const me = useMe().data;
  const id = d.chat.id;
  const memberInfo = usePresence({ users: d.members.map((m) => m.user_id) });
  const [title, setTitle] = useState(d.chat.title);
  const [history, setHistory] = useState(false);
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["chat", id] });
    void qc.invalidateQueries({ queryKey: ["chats"] });
    void qc.invalidateQueries({ queryKey: ["chat-member-events", id] });
  };
  const act = useMutation({
    mutationFn: (f: () => Promise<unknown>) => f(),
    onSuccess: refresh,
  });
  const leave = useMutation({
    mutationFn: () => ok(api.POST("/api/chat/{id}/leave", { params: { path: { id } } })),
    onSuccess: () => {
      refresh();
      onClose();
      void nav({ to: "/chats" });
    },
  });
  const myRole = d.chat.role;
  return (
    <Dialog open onClose={onClose} title={d.chat.title}>
      <div className="grid gap-4">
        {d.can_manage && (
          <Field label={t("chat.title")}>
            <div className="flex gap-2">
              <Input value={title} maxLength={200} onChange={(e) => setTitle(e.target.value)} />
              <Button disabled={!title.trim() || title === d.chat.title || act.isPending} onClick={() => act.mutate(() => ok(api.PUT("/api/chat/{id}/title", { params: { path: { id } }, body: { title } })))}>
                {t("common.save")}
              </Button>
            </div>
          </Field>
        )}
        <div>
          <h3 className="mb-1 text-sm font-semibold">{t("chat.membersN", { n: d.members.length })}</h3>
          <ul className="divide-y divide-line rounded-lg border border-line">
            {d.members.map((m) => {
              const pm = memberInfo.byUser.get(m.user_id);
              const removable = d.can_manage && m.user_id !== me?.user_id && m.role !== "owner" && (myRole === "owner" || m.role === "member");
              return (
                <li key={m.user_id} className="flex items-center gap-2 px-3 py-1.5 text-sm">
                  <UserAvatar name={m.name} userId={m.user_id} avatar={pm?.avatar} presence={pm?.presence} size="xs" />
                  <Link to="/people/$id" params={{ id: m.user_id }} className="min-w-0 flex-1 truncate hover:underline" onClick={onClose}>
                    {m.name}
                  </Link>
                  {m.role !== "member" && <Badge tone={m.role === "owner" ? "accent" : "muted"}>{dyn(t, `chat.roles.${m.role}`)}</Badge>}
                  {d.can_manage && m.role !== "owner" && m.user_id !== me?.user_id && (
                    <MenuButton
                      iconOnly
                      variant="ghost"
                      label={`${t("common.more")}: ${m.name}`}
                      items={[
                        m.role === "admin"
                          ? { key: "member", label: t("chat.unmakeAdmin"), onSelect: () => act.mutate(() => ok(api.PUT("/api/chat/{id}/members/{user}/role", { params: { path: { id, user: m.user_id } }, body: { role: "member" } }))) }
                          : { key: "admin", label: t("chat.makeAdmin"), onSelect: () => act.mutate(() => ok(api.PUT("/api/chat/{id}/members/{user}/role", { params: { path: { id, user: m.user_id } }, body: { role: "admin" } }))) },
                        { key: "remove", label: t("chat.remove"), danger: true, disabled: !removable, onSelect: () => act.mutate(() => ok(api.DELETE("/api/chat/{id}/members/{user}", { params: { path: { id, user: m.user_id } } }))) },
                      ]}
                    />
                  )}
                </li>
              );
            })}
          </ul>
        </div>
        {d.can_manage && (
          <Field group label={t("chat.addMembers")} hint={t("chat.addHint")}>
            <UserPicker exclude={d.members.map((m) => m.user_id)} onPick={(p) => act.mutate(() => ok(api.POST("/api/chat/{id}/members", { params: { path: { id } }, body: { user_ids: [p.id] } })))} />
          </Field>
        )}
        {d.can_manage && (
          <div>
            <button type="button" className="text-sm text-accent" aria-expanded={history} onClick={() => setHistory(!history)}>
              {t("chat.memberHistory")}
            </button>
            {history && (
              <div className="mt-2">
                <MemberHistory chat={id} />
              </div>
            )}
          </div>
        )}
        {(act.error ?? leave.error) && <ErrorBox error={act.error ?? leave.error} />}
        <div className="flex justify-between gap-2">
          {myRole !== "owner" ? (
            <Button variant="danger" disabled={leave.isPending} onClick={() => leave.mutate()}>
              {t("chat.leave")}
            </Button>
          ) : (
            <p className="text-xs text-muted">{t("chat.ownerHint")}</p>
          )}
          <Button onClick={onClose}>{t("common.close")}</Button>
        </div>
      </div>
    </Dialog>
  );
}

/** Переслать сообщение в другую переписку (с вложениями). После пересылки открывается та переписка. */
export function ForwardDialog({ chat, message, onClose }: { chat: string; message: ChatMessage; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const nav = useNavigate();
  const [q, setQ] = useState("");
  const list = useChats();
  const peers = usePresence({ users: (list.data ?? []).flatMap((c) => (c.peer_id ? [c.peer_id] : [])) });
  const needle = q.trim().toLowerCase();
  const targets = (list.data ?? []).filter((c) => c.active && !c.locked && (!needle || c.title.toLowerCase().includes(needle)));
  const send = useMutation({
    mutationFn: (to: string) => ok(api.POST("/api/chat/{id}/messages/{message}/forward", { params: { path: { id: chat, message: message.id } }, body: { to } })),
    onSuccess: (_, to) => {
      void qc.invalidateQueries({ queryKey: ["chats"] });
      void qc.invalidateQueries({ queryKey: ["chat-messages", to] });
      toast(t("chat.forwarded"));
      onClose();
      if (to !== chat) void nav({ to: "/chats/$id", params: { id: to } });
    },
  });
  return (
    <Dialog open onClose={onClose} title={t("chat.forwardTo")}>
      <Input type="search" autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("chat.searchChats")} aria-label={t("chat.searchChats")} />
      <div className="mt-2 max-h-80 overflow-y-auto">
        {list.error ? (
          <ErrorBox error={list.error} />
        ) : list.isPending ? (
          <Spinner variant="list" />
        ) : targets.length === 0 ? (
          <Empty>{t("chat.nothingFound")}</Empty>
        ) : (
          <ul className="divide-y divide-line">
            {targets.map((c) => {
              const p = c.peer_id ? peers.byUser.get(c.peer_id) : undefined;
              return (
                <li key={c.id}>
                  <button type="button" disabled={send.isPending} className="flex w-full items-center gap-3 px-2 py-2 text-left text-sm hover:bg-hover disabled:opacity-50" onClick={() => send.mutate(c.id)}>
                    <UserAvatar name={c.title} userId={c.peer_id} avatar={p?.avatar} presence={p?.presence} size="sm" />
                    <span className="min-w-0 flex-1 truncate font-medium">{c.title}</span>
                    {c.id === chat && <span className="text-xs text-muted">{t("chat.thisChat")}</span>}
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
      {send.error && <ErrorBox error={send.error} />}
    </Dialog>
  );
}
