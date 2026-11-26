// Коммуникации: список переписок слева, переписка справа (на телефоне — по очереди).
// Поведение — как в привычных мессенджерах: закреплённые сверху, затем по последнему сообщению;
// Enter отправляет, Shift+Enter — новая строка; своё сообщение можно править 15 минут.
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type ChatDetail, type ChatItem, type ChatMessage, type ChatPerson, type ChatMemberEvent, type ChatMessageVersion } from "../../api/client";
import { formatDate } from "../../format";
import { IconArrowLeft, IconPlus, IconStar } from "../../icons";
import { useMe } from "../../session";
import { Badge, Button, Card, Dialog, Empty, ErrorBox, Field, Input, MenuButton, Spinner } from "../../ui";
import { dyn } from "../helpdesk/helpdeskUtil";
import { useChats } from "./chatUtil";

const area = "w-full resize-none rounded-lg border border-control bg-paper px-3 py-2 text-sm focus:border-accent";

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

function NewChatDialog({ mode, onClose }: { mode: "direct" | "group"; onClose: () => void }) {
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

function ChatRow({ c, active }: { c: ChatItem; active: boolean }) {
  const { t, i18n } = useTranslation();
  return (
    <li>
      <Link
        to="/chats/$id"
        params={{ id: c.id }}
        className={`flex items-start gap-2 border-l-2 px-3 py-2.5 text-sm hover:bg-hover ${active ? "border-accent bg-hover" : "border-transparent"}`}
        aria-current={active ? "page" : undefined}
      >
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1.5">
            {c.pinned && (
              <>
                <IconStar size={12} className="text-accent" />
                <span className="sr-only">{t("chat.pinned")}</span>
              </>
            )}
            <span className={`truncate ${c.unread > 0 ? "font-semibold" : "font-medium"}`}>{c.title}</span>
          </div>
          <div className="truncate text-xs text-muted">{c.locked ? t("chat.locked") : (c.preview ?? (c.kind === "group" ? t("chat.group") : t("chat.direct")))}</div>
          {!c.active && (
            <div className="mt-0.5">
              <Badge>{t("chat.readOnly")}</Badge>
            </div>
          )}
        </div>
        <div className="flex shrink-0 flex-col items-end gap-1">
          {c.last_message_at && <span className="text-[11px] text-muted">{formatDate(c.last_message_at, i18n.language)}</span>}
          {c.unread > 0 && !c.locked && (
            <span className="rounded-full bg-accent px-1.5 text-xs font-medium text-paper tabular-nums" aria-label={t("chat.unread", { count: c.unread })}>
              {c.unread}
            </span>
          )}
        </div>
      </Link>
    </li>
  );
}

function ChatList({ current }: { current?: string }) {
  const { t } = useTranslation();
  const [hidden, setHidden] = useState(false);
  const [creating, setCreating] = useState<"direct" | "group" | null>(null);
  const q = useChats(hidden);
  return (
    <Card className="flex min-h-0 flex-col">
      <div className="flex items-center justify-between gap-2 border-b border-line px-3 py-2">
        <h2 className="font-semibold">{hidden ? t("chat.hiddenTitle") : t("chat.title_list")}</h2>
        <MenuButton
          label={t("chat.new")}
          icon={<IconPlus size={16} />}
          items={[
            { key: "direct", label: t("chat.newDirect"), onSelect: () => setCreating("direct") },
            { key: "group", label: t("chat.newGroup"), onSelect: () => setCreating("group") },
          ]}
        />
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {q.error ? (
          <ErrorBox error={q.error} onRetry={() => void q.refetch()} />
        ) : q.isPending ? (
          <Spinner variant="list" />
        ) : q.data.length === 0 ? (
          <Empty hint={hidden ? undefined : t("chat.emptyHint")}>{hidden ? t("chat.noHidden") : t("chat.empty")}</Empty>
        ) : (
          <ul className="divide-y divide-line">
            {q.data.map((c) => (
              <ChatRow key={c.id} c={c} active={c.id === current} />
            ))}
          </ul>
        )}
      </div>
      <button type="button" className="border-t border-line px-3 py-2 text-left text-xs text-accent" onClick={() => setHidden(!hidden)}>
        {hidden ? t("chat.showMain") : t("chat.showHidden")}
      </button>
      {creating && <NewChatDialog mode={creating} onClose={() => setCreating(null)} />}
    </Card>
  );
}

function Versions({ chat, message, onClose }: { chat: string; message: ChatMessage; onClose: () => void }) {
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

function ChatInfo({ d, onClose }: { d: ChatDetail; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const nav = useNavigate();
  const me = useMe().data;
  const id = d.chat.id;
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
              const removable = d.can_manage && m.user_id !== me?.user_id && m.role !== "owner" && (myRole === "owner" || m.role === "member");
              return (
                <li key={m.user_id} className="flex items-center gap-2 px-3 py-1.5 text-sm">
                  <span className="min-w-0 flex-1 truncate">{m.name}</span>
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

function Bubble({ m, mine, group, canVersions, onEdit, onVersions }: { m: ChatMessage; mine: boolean; group: boolean; canVersions: boolean; onEdit: () => void; onVersions: () => void }) {
  const { t, i18n } = useTranslation();
  return (
    <li className={`flex ${mine ? "justify-end" : "justify-start"}`}>
      <div className={`max-w-[85%] rounded-2xl px-3 py-1.5 text-sm sm:max-w-[70%] ${mine ? "bg-accent/15" : "bg-hover"}`}>
        {group && !mine && <div className="text-xs font-medium text-accent">{m.author_name}</div>}
        <p className="break-words whitespace-pre-wrap">{m.body}</p>
        <div className="mt-0.5 flex items-center justify-end gap-2 text-[11px] text-muted">
          {m.version > 1 &&
            (canVersions ? (
              <button type="button" className="underline" onClick={onVersions}>
                {t("chat.edited")}
              </button>
            ) : (
              <span>{t("chat.edited")}</span>
            ))}
          {m.editable && (
            <button type="button" className="underline" onClick={onEdit}>
              {t("common.edit")}
            </button>
          )}
          <time dateTime={m.created_at}>{formatDate(m.created_at, i18n.language)}</time>
        </div>
      </div>
    </li>
  );
}

/** Неотправленные сообщения по чатам: не теряются, когда открывают правку другого сообщения или другой чат. */
const drafts = new Map<string, string>();

function Composer({ chat, editing, onDone }: { chat: string; editing: ChatMessage | null; onDone: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [text, setTextState] = useState(editing ? editing.body : (drafts.get(chat) ?? ""));
  const setText = (v: string) => {
    setTextState(v);
    if (!editing) {
      if (v) drafts.set(chat, v);
      else drafts.delete(chat);
    }
  };
  const box = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    box.current?.focus();
  }, [editing]);
  const send = useMutation({
    mutationFn: (body: string) =>
      editing
        ? ok(api.PUT("/api/chat/{id}/messages/{message}", { params: { path: { id: chat, message: editing.id } }, body: { body } }))
        : ok(api.POST("/api/chat/{id}/messages", { params: { path: { id: chat } }, body: { body } })),
    onSuccess: () => {
      setText("");
      onDone();
      void qc.invalidateQueries({ queryKey: ["chat-messages", chat] });
      void qc.invalidateQueries({ queryKey: ["chats"] });
    },
  });
  const submit = () => {
    if (text.trim() && !send.isPending) send.mutate(text);
  };
  return (
    <div className="border-t border-line p-2">
      {editing && (
        <div className="mb-1 flex items-center justify-between text-xs text-muted">
          <span>{t("chat.editing")}</span>
          <button type="button" className="text-accent" onClick={onDone}>
            {t("common.cancel")}
          </button>
        </div>
      )}
      {send.error && <ErrorBox error={send.error} />}
      <div className="flex items-end gap-2">
        <textarea
          ref={box}
          rows={2}
          maxLength={4000}
          value={text}
          aria-label={t("chat.message")}
          placeholder={t("chat.placeholder")}
          className={area}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              submit();
            }
            if (e.key === "Escape" && editing) onDone();
          }}
        />
        <Button variant="primary" disabled={!text.trim() || send.isPending} onClick={submit}>
          {editing ? t("common.save") : t("chat.send")}
        </Button>
      </div>
    </div>
  );
}

function Conversation({ id }: { id: string }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const me = useMe().data;
  const [info, setInfo] = useState(false);
  const [editing, setEditing] = useState<ChatMessage | null>(null);
  const [versions, setVersions] = useState<ChatMessage | null>(null);
  const detail = useQuery({ queryKey: ["chat", id], queryFn: () => ok<ChatDetail>(api.GET("/api/chat/{id}", { params: { path: { id } } })) });
  const locked = detail.data?.chat.locked === true;
  const messages = useInfiniteQuery({
    queryKey: ["chat-messages", id],
    queryFn: ({ pageParam }) => ok<ChatMessage[]>(api.GET("/api/chat/{id}/messages", { params: { path: { id }, query: { before: pageParam } } })),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => (last.length >= 100 ? last[0]?.id : undefined),
    enabled: detail.isSuccess && !locked,
  });
  const all = [...(messages.data?.pages ?? [])].reverse().flat();
  const newest = all[all.length - 1]?.id;
  const unread = detail.data?.chat.unread ?? 0;
  // Открытая переписка прочитана: и при входе, и когда приходит новое сообщение.
  const read = useMutation({
    mutationFn: () => ok(api.POST("/api/chat/{id}/read", { params: { path: { id } } })),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["chats"] });
      void qc.invalidateQueries({ queryKey: ["chat", id] });
    },
  });
  const readMutate = read.mutate;
  useEffect(() => {
    if (newest && unread > 0 && document.visibilityState === "visible") readMutate();
  }, [newest, unread, readMutate]);
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [newest]);
  const prefs = useMutation({
    mutationFn: (body: { pinned?: boolean; hidden?: boolean }) => ok(api.PUT("/api/chat/{id}/prefs", { params: { path: { id } }, body })),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["chats"] });
      void qc.invalidateQueries({ queryKey: ["chat", id] });
    },
  });
  if (detail.error) return <ErrorBox error={detail.error} />;
  if (detail.isPending) return <Spinner />;
  const d = detail.data;
  const c = d.chat;
  const group = c.kind === "group";
  const manager = c.active && (c.role === "owner" || c.role === "admin");
  return (
    <Card className="flex min-h-0 flex-col">
      <div className="flex items-center gap-2 border-b border-line px-3 py-2">
        <Link to="/chats" className="rounded-lg p-1 text-muted hover:bg-hover md:hidden" aria-label={t("chat.back")}>
          <IconArrowLeft size={18} />
        </Link>
        <div className="min-w-0 flex-1">
          <h2 className="truncate font-semibold">{c.title}</h2>
          <div className="text-xs text-muted">{group ? (c.active ? t("chat.membersN", { n: d.members.length }) : t("chat.readOnly")) : t("chat.direct")}</div>
        </div>
        {(c.kind === "direct" || c.active) && (
          <Button variant="ghost" disabled={prefs.isPending} onClick={() => prefs.mutate({ pinned: !c.pinned })}>
            {c.pinned ? t("chat.unpin") : t("chat.pin")}
          </Button>
        )}
        {group && !c.active && (
          <Button variant="ghost" disabled={prefs.isPending} onClick={() => prefs.mutate({ hidden: !c.hidden })}>
            {c.hidden ? t("chat.unhide") : t("chat.hide")}
          </Button>
        )}
        {group && c.active && !locked && <Button onClick={() => setInfo(true)}>{t("chat.info")}</Button>}
      </div>
      {prefs.error && <ErrorBox error={prefs.error} />}
      {locked ? (
        <Empty hint={t("chat.lockedHint")}>{t("chat.locked")}</Empty>
      ) : (
        <>
          <div className="min-h-0 flex-1 overflow-y-auto px-3 py-2" aria-live="polite" aria-relevant="additions">
            {messages.hasNextPage && (
              <div className="mb-2 text-center">
                <Button variant="ghost" disabled={messages.isFetchingNextPage} onClick={() => void messages.fetchNextPage()}>
                  {t("chat.older")}
                </Button>
              </div>
            )}
            {messages.error ? (
              <ErrorBox error={messages.error} />
            ) : messages.isPending ? (
              <Spinner />
            ) : all.length === 0 ? (
              <Empty>{t("chat.noMessages")}</Empty>
            ) : (
              <ul className="space-y-1.5">
                {all.map((m) => (
                  <Bubble
                    key={m.id}
                    m={m}
                    mine={m.author_id === me?.user_id}
                    group={group}
                    canVersions={!group || m.author_id === me?.user_id || manager}
                    onEdit={() => setEditing(m)}
                    onVersions={() => setVersions(m)}
                  />
                ))}
              </ul>
            )}
            <div ref={end} />
          </div>
          {d.can_write ? (
            <Composer key={editing?.id ?? "new"} chat={id} editing={editing} onDone={() => setEditing(null)} />
          ) : (
            <p className="border-t border-line px-3 py-2 text-sm text-muted">{t("chat.readOnlyHint")}</p>
          )}
        </>
      )}
      {info && <ChatInfo d={d} onClose={() => setInfo(false)} />}
      {versions && <Versions chat={id} message={versions} onClose={() => setVersions(null)} />}
    </Card>
  );
}

const shell = "grid h-[calc(100dvh-7.5rem)] min-h-96 gap-3 md:grid-cols-[20rem_1fr]";

export function ChatsPage() {
  const { t } = useTranslation();
  return (
    <div className={shell}>
      <ChatList />
      <Card className="hidden items-center justify-center md:flex">
        <p className="text-sm text-muted">{t("chat.pick")}</p>
      </Card>
    </div>
  );
}

export function ChatPage() {
  const { id } = useParams({ from: "/chats/$id" });
  return (
    <div className={shell}>
      <div className="hidden min-h-0 md:flex md:flex-col [&>*]:flex-1">
        <ChatList current={id} />
      </div>
      <Conversation key={id} id={id} />
    </div>
  );
}
