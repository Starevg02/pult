// Переписка как в привычных мессенджерах: пузыри с хвостиком, разделители по дням, «Новые сообщения»,
// ответы и пересылка, реакции, закреплённое сверху, «печатает…», ✓/✓✓ прочитано, поиск по переписке
// и все её файлы. Лента не прыгает вниз, пока человек читает старое: внизу появляется кнопка со счётчиком.
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type ChatDetail, type ChatMessage, type ChatSharedFile } from "../../api/client";
import { formatDate, formatDay, formatTime } from "../../format";
import {
  IconArrowLeft,
  IconBell,
  IconBellOff,
  IconCheck,
  IconCheckDouble,
  IconChevronDown,
  IconCopy,
  IconFileText,
  IconMore,
  IconPin,
  IconReply,
  IconSearch,
  IconSmile,
  IconStar,
  IconTransfer,
  IconX,
} from "../../icons";
import { Lightbox } from "../../lightbox";
import { useMe } from "../../session";
import { toast } from "../../toast";
import { Empty, ErrorBox, Input, MenuButton, Spinner } from "../../ui";
import { UserAvatar } from "../people/presence";
import { usePresence } from "../people/profileHooks";
import { formatSize } from "../helpdesk/helpdeskUtil";
import { ChatInfo, ForwardDialog, Versions } from "./ChatDialogs";
import { Attachments, MessageText, Quote, ReactionPicker, Reactions } from "./ChatParts";
import { Composer } from "./Composer";
import { chatFileUrl, dayKey, onlyEmoji, uploadPending, MAX_FILES, useTyping } from "./chatUtil";

/** Подряд идущие сообщения одного автора в пределах 5 минут — одной группой (имя и аватар один раз). */
const RUN_MS = 5 * 60_000;
/** Сколько страниц истории догружать, чтобы дойти до сообщения из поиска или закреплённого. */
const JUMP_PAGES = 30;

const hasFiles = (e: { dataTransfer: DataTransfer }) => [...e.dataTransfer.types].includes("Files");

type Panel = "search" | "files" | null;

export function Conversation({ id }: { id: string }) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const me = useMe().data;
  const [info, setInfo] = useState(false);
  const [panel, setPanel] = useState<Panel>(null);
  const [editing, setEditing] = useState<ChatMessage | null>(null);
  const [reply, setReply] = useState<ChatMessage | null>(null);
  const [versions, setVersions] = useState<ChatMessage | null>(null);
  const [forward, setForward] = useState<ChatMessage | null>(null);
  const [dragging, setDragging] = useState(false);
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
  const newest = all[all.length - 1];
  const unread = detail.data?.chat.unread ?? 0;

  // Открытая переписка прочитана: и при входе, и когда приходит новое сообщение (если вкладка на экране).
  const read = useMutation({
    mutationFn: () => ok(api.POST("/api/chat/{id}/read", { params: { path: { id } } })),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["chats"] });
      void qc.invalidateQueries({ queryKey: ["chat", id] });
    },
  });
  const readMutate = read.mutate;
  useEffect(() => {
    const back = () => {
      if (document.visibilityState === "visible" && (qc.getQueryData<ChatDetail>(["chat", id])?.chat.unread ?? 0) > 0) readMutate();
    };
    document.addEventListener("visibilitychange", back);
    return () => document.removeEventListener("visibilitychange", back);
  }, [id, qc, readMutate]);

  // Граница «Новые сообщения» — по непрочитанному в момент открытия (запоминается при первом показе).
  const [unreadAtOpen, setUnreadAtOpen] = useState<number | null>(null);
  if (unreadAtOpen === null && detail.data && detail.isFetchedAfterMount) setUnreadAtOpen(detail.data.chat.unread);
  const [firstUnread, setFirstUnread] = useState<string | null | undefined>(undefined);
  if (firstUnread === undefined && unreadAtOpen !== null && messages.isSuccess && messages.isFetchedAfterMount && !messages.isFetching && me) {
    let left = unreadAtOpen;
    let found: string | null = null;
    for (let i = all.length - 1; i >= 0 && left > 0; i--) {
      const m = all[i];
      if (m && m.author_id !== me.user_id) {
        left--;
        found = m.id;
      }
    }
    setFirstUnread(found);
  }
  const latched = firstUnread !== undefined;
  useEffect(() => {
    if (latched && newest && unread > 0 && document.visibilityState === "visible") readMutate();
  }, [latched, newest, unread, readMutate]);

  // ---------------------------------------------------------------- прокрутка
  const scroller = useRef<HTMLDivElement>(null);
  const nearBottom = useRef(true);
  const started = useRef(false);
  const jumping = useRef(false);
  const lastNewest = useRef<string | undefined>(undefined);
  const heightBefore = useRef<number | null>(null);
  const [atBottom, setAtBottom] = useState(true);
  // Последнее сообщение, до которого человек долистал: всё, что новее, — на счётчике кнопки «вниз».
  const [seen, setSeen] = useState<string | undefined>(undefined);
  const seenAt = seen ? all.findIndex((m) => m.id === seen) : -1;
  const missed = atBottom || seenAt < 0 ? 0 : all.slice(seenAt + 1).filter((m) => m.author_id !== me?.user_id).length;

  // Первый показ: к «Новым сообщениям» или в самый низ.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (started.current || !el || firstUnread === undefined || all.length === 0) return;
    started.current = true;
    lastNewest.current = newest?.id;
    const mark = firstUnread ? document.getElementById("chat-unread-mark") : null;
    if (mark) mark.scrollIntoView({ block: "start" });
    else el.scrollTop = el.scrollHeight;
  }, [all.length, firstUnread, newest?.id]);

  // Новое сообщение: вниз, если человек и так внизу или написал сам; иначе — счётчик на кнопке.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!started.current || !el || !newest || newest.id === lastNewest.current) return;
    lastNewest.current = newest.id;
    if (nearBottom.current || newest.author_id === me?.user_id) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, [newest, me?.user_id]);

  // Подгрузили старые сообщения сверху — лента остаётся на том же месте.
  const pageCount = messages.data?.pages.length ?? 0;
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && heightBefore.current !== null) {
      el.scrollTop += el.scrollHeight - heightBefore.current;
      heightBefore.current = null;
    }
  }, [pageCount]);

  const toBottom = () => {
    const el = scroller.current;
    if (el) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  };

  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    nearBottom.current = bottom;
    if (bottom !== atBottom) setAtBottom(bottom);
    if (bottom && newest && newest.id !== seen) setSeen(newest.id);
    if (!bottom && seen === undefined && newest) setSeen(newest.id);
    // У верхнего края — догрузить старое.
    if (el.scrollTop < 200 && started.current && !jumping.current && messages.hasNextPage && !messages.isFetchingNextPage) {
      heightBefore.current = el.scrollHeight;
      void messages.fetchNextPage();
    }
  };

  // Переход к сообщению (поиск, закреплённое, цитата): догружаем историю, пока не найдётся.
  const [flash, setFlash] = useState<string | null>(null);
  const { fetchNextPage } = messages;
  const jumpTo = async (mid: string) => {
    if (!mid || jumping.current) return;
    jumping.current = true;
    try {
      let more = messages.hasNextPage;
      for (let n = 0; n <= JUMP_PAGES; n++) {
        const el = document.getElementById(`msg-${mid}`);
        if (el) {
          el.scrollIntoView({ block: "center", behavior: "smooth" });
          setFlash(mid);
          window.setTimeout(() => setFlash((f) => (f === mid ? null : f)), 1800);
          return;
        }
        if (!more) break;
        heightBefore.current = null;
        const r = await fetchNextPage();
        more = r.hasNextPage;
        await new Promise<void>((done) => requestAnimationFrame(() => requestAnimationFrame(() => done())));
      }
      toast(t("chat.notLoaded"), "bad");
    } finally {
      jumping.current = false;
    }
  };
  const jump = (mid: string) => void jumpTo(mid);

  // ---------------------------------------------------------------- действия
  const refreshMessages = () => void qc.invalidateQueries({ queryKey: ["chat-messages", id] });
  const prefs = useMutation({
    mutationFn: (body: { pinned?: boolean; hidden?: boolean; muted?: boolean }) => ok(api.PUT("/api/chat/{id}/prefs", { params: { path: { id } }, body })),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["chats"] });
      void qc.invalidateQueries({ queryKey: ["chat", id] });
    },
  });
  const react = useMutation({
    mutationFn: ({ message, emoji }: { message: string; emoji: string }) =>
      ok(api.PUT("/api/chat/{id}/messages/{message}/reactions", { params: { path: { id, message } }, body: { emoji } })),
    onSuccess: refreshMessages,
    onError: (e) => toast(e instanceof Error ? e.message : t("common.error"), "bad"),
  });
  const pin = useMutation({
    mutationFn: (message: string | null) => ok(api.PUT("/api/chat/{id}/pin", { params: { path: { id } }, body: { message } })),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["chat", id] }),
    onError: (e) => toast(e instanceof Error ? e.message : t("common.error"), "bad"),
  });

  const members = detail.data?.members ?? [];
  const typingIds = useTyping(id).filter((u) => u !== me?.user_id);
  const memberInfo = usePresence({ users: detail.data?.chat.kind === "group" ? members.map((m) => m.user_id) : detail.data?.chat.peer_id ? [detail.data.chat.peer_id] : [] });

  if (detail.error) return <ErrorBox error={detail.error} />;
  if (detail.isPending) return <Spinner />;
  const d = detail.data;
  const c = d.chat;
  const group = c.kind === "group";
  const peer = c.peer_id ? memberInfo.byUser.get(c.peer_id) : undefined;
  const manager = c.active && (c.role === "owner" || c.role === "admin");
  const readUpto = d.read_upto ? new Date(d.read_upto).getTime() : 0;
  const typingNames = typingIds.map((u) => members.find((m) => m.user_id === u)?.name ?? "…");
  const incoming = newest && newest.author_id !== me?.user_id ? `${newest.author_name}: ${newest.body.split("\n")[0] ?? ""}` : "";

  const subtitle = typingNames.length
    ? group
      ? t("chat.typingNamed", { names: typingNames.slice(0, 2).join(", "), count: typingNames.length })
      : t("chat.typing")
    : group
      ? c.active
        ? t("chat.membersN", { n: members.length })
        : t("chat.readOnly")
      : peer && (peer.status_emoji || peer.status_text)
        ? `${peer.status_emoji ?? ""} ${peer.status_text ?? ""}`.trim()
        : peer
          ? t(`people.presence.${peer.presence}`)
          : t("chat.direct");

  const canWrite = d.can_write && !locked;
  const addFiles = (list: File[]) => {
    if (canWrite && !editing && list.length > 0) void uploadPending(id, list, { tooMany: t("chat.tooManyFiles", { count: MAX_FILES }), failed: t("common.error") });
  };
  const editLast = () => {
    const mine = [...all].reverse().find((m) => m.author_id === me?.user_id && m.editable && m.body);
    if (mine) setEditing(mine);
  };

  return (
    <section
      className="relative flex min-h-0 flex-col overflow-hidden rounded-xl border border-line bg-paper"
      aria-label={c.title}
      onDragEnter={(e) => {
        if (hasFiles(e) && canWrite && !editing) setDragging(true);
      }}
      onDragOver={(e) => {
        if (hasFiles(e)) e.preventDefault();
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target || !e.currentTarget.contains(e.relatedTarget as Node)) setDragging(false);
      }}
      onDrop={(e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        setDragging(false);
        addFiles([...e.dataTransfer.files]);
      }}
    >
      {/* Шапка */}
      <header className="flex items-center gap-2 border-b border-line bg-paper/95 px-3 py-2 backdrop-blur">
        <Link to="/chats" className="rounded-lg p-1 text-muted hover:bg-hover md:hidden" aria-label={t("chat.back")}>
          <IconArrowLeft size={18} />
        </Link>
        <UserAvatar name={c.title} userId={c.peer_id} avatar={peer?.avatar} presence={peer?.presence} size="md" />
        <div className="min-w-0 flex-1">
          {c.peer_id ? (
            <Link to="/people/$id" params={{ id: c.peer_id }} className="block truncate font-semibold hover:underline">
              {c.title}
            </Link>
          ) : group && c.active && !locked ? (
            <button type="button" className="block max-w-full truncate text-left font-semibold hover:underline" onClick={() => setInfo(true)}>
              {c.title}
            </button>
          ) : (
            <h2 className="truncate font-semibold">{c.title}</h2>
          )}
          <div className={`truncate text-xs ${typingNames.length ? "text-accent" : "text-muted"}`} aria-live="polite">
            {typingNames.length > 0 && <span className="chat-typing-dots mr-1" aria-hidden="true" />}
            {subtitle}
          </div>
        </div>
        {!locked && (
          <>
            <button type="button" className={`rounded-lg p-2 hover:bg-hover ${panel === "search" ? "bg-hover text-accent" : "text-muted"}`} aria-pressed={panel === "search"} aria-label={t("chat.searchInChat")} title={t("chat.searchInChat")} onClick={() => setPanel(panel === "search" ? null : "search")}>
              <IconSearch size={18} />
            </button>
            <button type="button" className={`rounded-lg p-2 hover:bg-hover ${panel === "files" ? "bg-hover text-accent" : "text-muted"}`} aria-pressed={panel === "files"} aria-label={t("chat.sharedFiles")} title={t("chat.sharedFiles")} onClick={() => setPanel(panel === "files" ? null : "files")}>
              <IconFileText size={18} />
            </button>
          </>
        )}
        <MenuButton
          iconOnly
          variant="ghost"
          label={t("common.more")}
          icon={<IconMore size={18} />}
          items={[
            { key: "mute", label: c.muted ? t("chat.unmute") : t("chat.mute"), icon: c.muted ? <IconBell size={16} /> : <IconBellOff size={16} />, onSelect: () => prefs.mutate({ muted: !c.muted }) },
            ...(c.kind === "direct" || c.active ? [{ key: "pin", label: c.pinned ? t("chat.unpin") : t("chat.pin"), icon: <IconStar size={16} />, onSelect: () => prefs.mutate({ pinned: !c.pinned }) }] : []),
            ...(group && !c.active ? [{ key: "hide", label: c.hidden ? t("chat.unhide") : t("chat.hide"), onSelect: () => prefs.mutate({ hidden: !c.hidden }) }] : []),
            ...(group && c.active && !locked ? [{ key: "info", label: t("chat.info"), onSelect: () => setInfo(true), separated: true }] : []),
          ]}
        />
      </header>
      {prefs.error && <ErrorBox error={prefs.error} />}

      {/* Закреплённое */}
      {d.pinned_message && !locked && (
        <div className="flex items-center gap-2 border-b border-line bg-paper px-3 py-1.5">
          <IconPin size={16} className="text-accent" />
          <div className="min-w-0 flex-1">
            <Quote q={d.pinned_message} label={t("chat.pinnedMessage")} onClick={() => jump(d.pinned_message?.id ?? "")} onClear={d.can_pin ? () => pin.mutate(null) : undefined} />
          </div>
        </div>
      )}

      {locked ? (
        <Empty hint={t("chat.lockedHint")}>{t("chat.locked")}</Empty>
      ) : (
        <div className="relative flex min-h-0 flex-1">
          <div className="relative flex min-w-0 flex-1 flex-col">
            <div ref={scroller} onScroll={onScroll} className="chat-wallpaper min-h-0 flex-1 overflow-y-auto px-2 py-3 sm:px-4" aria-label={t("chat.messages")}>
              {messages.isFetchingNextPage && (
                <div className="mb-2 text-center text-xs text-muted" role="status">
                  {t("chat.loadingOlder")}
                </div>
              )}
              {messages.error ? (
                <ErrorBox error={messages.error} />
              ) : messages.isPending ? (
                <Spinner />
              ) : all.length === 0 ? (
                <div className="flex h-full items-center justify-center">
                  <div className="rounded-2xl bg-paper/90 px-6 py-5 text-center shadow-sm">
                    <div className="mb-1 text-4xl" aria-hidden="true">
                      👋
                    </div>
                    <p className="font-medium">{t("chat.noMessages")}</p>
                    <p className="text-sm text-muted">{t("chat.sayHello")}</p>
                  </div>
                </div>
              ) : (
                <ol className="mx-auto max-w-3xl">
                  {all.map((m, i) => {
                    const prev = all[i - 1];
                    const next = all[i + 1];
                    const newDay = !prev || dayKey(prev.created_at) !== dayKey(m.created_at);
                    const runStart = newDay || !prev || prev.author_id !== m.author_id || new Date(m.created_at).getTime() - new Date(prev.created_at).getTime() > RUN_MS || m.id === firstUnread;
                    const runEnd = !next || next.author_id !== m.author_id || dayKey(next.created_at) !== dayKey(m.created_at) || new Date(next.created_at).getTime() - new Date(m.created_at).getTime() > RUN_MS || next.id === firstUnread;
                    const mine = m.author_id === me?.user_id;
                    return (
                      <li key={m.id}>
                        {newDay && (
                          <div className="sticky top-0 z-[1] my-2 flex justify-center">
                            <span className="rounded-full bg-ink/40 px-3 py-0.5 text-xs font-medium text-white backdrop-blur-sm">{formatDay(m.created_at, i18n.language)}</span>
                          </div>
                        )}
                        {m.id === firstUnread && (
                          <div id="chat-unread-mark" className="my-2 scroll-mt-10 bg-paper/70 py-1 text-center text-xs font-medium text-accent">
                            {t("chat.newMessages")}
                          </div>
                        )}
                        <Bubble
                          m={m}
                          mine={mine}
                          group={group}
                          runStart={runStart}
                          runEnd={runEnd}
                          flash={flash === m.id}
                          read={mine && new Date(m.created_at).getTime() <= readUpto}
                          avatar={memberInfo.byUser.get(m.author_id)?.avatar}
                          canWrite={canWrite}
                          canPin={d.can_pin}
                          pinned={d.pinned_message?.id === m.id}
                          canVersions={!group || mine || manager}
                          onReply={() => {
                            setEditing(null);
                            setReply(m);
                          }}
                          onEdit={() => {
                            setReply(null);
                            setEditing(m);
                          }}
                          onVersions={() => setVersions(m)}
                          onForward={() => setForward(m)}
                          onPin={() => pin.mutate(d.pinned_message?.id === m.id ? null : m.id)}
                          onReact={(emoji) => react.mutate({ message: m.id, emoji })}
                          onJump={jump}
                        />
                      </li>
                    );
                  })}
                </ol>
              )}
            </div>
            <p className="sr-only" aria-live="polite">
              {incoming}
            </p>
            {!atBottom && (
              <button type="button" onClick={() => toBottom()} className="absolute right-4 bottom-20 z-10 flex size-11 items-center justify-center rounded-full border border-line bg-paper text-muted shadow-lg hover:text-ink" aria-label={missed ? t("chat.newBelow", { count: missed }) : t("chat.toBottom")}>
                <IconChevronDown size={20} />
                {missed > 0 && <span className="absolute -top-1.5 -right-1 min-w-5 rounded-full bg-accent px-1 text-xs font-semibold text-paper tabular-nums">{missed}</span>}
              </button>
            )}
            {canWrite ? (
              <Composer
                key={editing?.id ?? "new"}
                chat={id}
                editing={editing}
                reply={reply}
                onAddFiles={addFiles}
                onEditLast={editLast}
                onJump={jump}
                onDone={() => {
                  setEditing(null);
                  setReply(null);
                }}
                onCancelReply={() => setReply(null)}
              />
            ) : (
              <p className="border-t border-line bg-paper px-3 py-3 text-center text-sm text-muted">{t("chat.readOnlyHint")}</p>
            )}
            {dragging && (
              <div className="pointer-events-none absolute inset-2 z-20 flex items-center justify-center rounded-2xl border-2 border-dashed border-accent bg-paper/85 text-lg font-medium text-accent">
                {t("chat.dropHere")}
              </div>
            )}
          </div>
          {panel === "search" && <SearchPanel chat={id} onClose={() => setPanel(null)} onJump={jump} />}
          {panel === "files" && <FilesPanel chat={id} onClose={() => setPanel(null)} onJump={jump} />}
        </div>
      )}
      {info && <ChatInfo d={d} onClose={() => setInfo(false)} />}
      {versions && <Versions chat={id} message={versions} onClose={() => setVersions(null)} />}
      {forward && <ForwardDialog chat={id} message={forward} onClose={() => setForward(null)} />}
    </section>
  );
}

// ------------------------------------------------------------------ Сообщение

type BubbleProps = {
  m: ChatMessage;
  mine: boolean;
  group: boolean;
  runStart: boolean;
  runEnd: boolean;
  flash: boolean;
  read: boolean;
  avatar?: string | null;
  canWrite: boolean;
  canPin: boolean;
  pinned: boolean;
  canVersions: boolean;
  onReply: () => void;
  onEdit: () => void;
  onVersions: () => void;
  onForward: () => void;
  onPin: () => void;
  onReact: (emoji: string) => void;
  onJump: (id: string) => void;
};

function Bubble(p: BubbleProps) {
  const { m, mine, group } = p;
  const { t, i18n } = useTranslation();
  const [picking, setPicking] = useState(false);
  // У верхнего края ленты выбор открывается вниз, иначе его обрежет прокрутка.
  const [below, setBelow] = useState(false);
  const smile = useRef<HTMLButtonElement>(null);
  const closePicker = useCallback(() => setPicking(false), []);
  const big = !m.reply && !m.forwarded_from_name && m.files.length === 0 && onlyEmoji(m.body);
  const showName = group && !mine && p.runStart;
  const showAvatar = group && !mine;
  const tail = p.runEnd && !big ? (mine ? "rounded-br-md" : "rounded-bl-md") : "";
  const copy = () => {
    void navigator.clipboard?.writeText(m.body).then(
      () => toast(t("chat.copied")),
      () => toast(t("common.error"), "bad"),
    );
  };
  const menu = [
    ...(p.canWrite ? [{ key: "reply", label: t("chat.reply"), icon: <IconReply size={16} />, onSelect: p.onReply }] : []),
    { key: "forward", label: t("chat.forward"), icon: <IconTransfer size={16} />, onSelect: p.onForward },
    ...(m.body ? [{ key: "copy", label: t("chat.copyText"), icon: <IconCopy size={16} />, onSelect: copy }] : []),
    ...(m.editable && m.body ? [{ key: "edit", label: t("common.edit"), onSelect: p.onEdit }] : []),
    ...(p.canPin ? [{ key: "pin", label: p.pinned ? t("chat.unpinMessage") : t("chat.pinMessage"), icon: <IconPin size={16} />, onSelect: p.onPin }] : []),
    ...(m.version > 1 && p.canVersions ? [{ key: "versions", label: t("chat.versions"), onSelect: p.onVersions, separated: true }] : []),
  ];
  const meta = (
    <span className={`ml-2 inline-flex translate-y-1 items-center gap-1 align-bottom text-[11px] whitespace-nowrap select-none ${big ? "rounded-full bg-ink/40 px-1.5 text-white" : mine ? "text-accent-ink/70" : "text-muted"} float-right`}>
      {m.version > 1 && <span>{t("chat.edited")}</span>}
      <time dateTime={m.created_at} title={formatDate(m.created_at, i18n.language)}>
        {formatTime(m.created_at, i18n.language)}
      </time>
      {mine &&
        (p.read ? (
          <><IconCheckDouble size={14} className="text-accent" /><span className="sr-only">{t("chat.read")}</span></>
        ) : (
          <><IconCheck size={14} /><span className="sr-only">{t("chat.sent")}</span></>
        ))}
    </span>
  );
  return (
    <div
      id={`msg-${m.id}`}
      className={`group/msg flex items-end gap-2 ${mine ? "justify-end" : "justify-start"} ${p.runStart ? "mt-2" : "mt-0.5"} scroll-my-20 rounded-xl transition-colors duration-700 ${p.flash ? "bg-accent/20" : ""}`}
    >
      {showAvatar && <span className="w-8 shrink-0">{p.runEnd && <UserAvatar name={m.author_name} userId={m.author_id} avatar={p.avatar} size="sm" />}</span>}
      <div className={`relative flex max-w-[88%] items-center gap-1 sm:max-w-[72%] ${mine ? "flex-row-reverse" : ""}`}>
        <div className={big ? "px-1" : `min-w-0 rounded-2xl px-3 py-1.5 text-[15px] leading-snug shadow-sm ${mine ? "chat-bubble-mine" : "chat-bubble-other"} ${tail}`}>
          {showName && !big && <div className="mb-0.5 truncate text-[13px] font-semibold text-accent">{m.author_name}</div>}
          {m.forwarded_from_name && (
            <div className="mb-1 flex items-center gap-1 text-xs text-accent">
              <IconTransfer size={12} /> {t("chat.forwardedFrom", { name: m.forwarded_from_name })}
            </div>
          )}
          {m.reply && (
            <div className="mb-1 rounded-lg bg-ink/5 py-1 pr-2">
              <Quote q={m.reply} onClick={() => p.onJump(m.reply?.id ?? "")} />
            </div>
          )}
          {m.files.length > 0 && <Attachments files={m.files} />}
          {big ? (
            <div className="text-5xl leading-tight">
              <span role="img" aria-label={m.body}>
                {m.body}
              </span>
              {meta}
            </div>
          ) : m.body ? (
            <div className="mt-0.5">
              <MessageText body={m.body} />
              {meta}
              <span className="clear-both block" />
            </div>
          ) : (
            <div className="flex justify-end">{meta}</div>
          )}
          <Reactions items={m.reactions} disabled={!p.canWrite} onToggle={p.onReact} />
        </div>
        {/* Действия: на мыши — при наведении, на сенсорном экране — всегда. */}
        <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover/msg:opacity-100 focus-within:opacity-100 [@media(hover:none)]:opacity-100">
          {p.canWrite && (
            <>
              <button
                ref={smile}
                type="button"
                className="rounded-full p-1.5 text-muted hover:bg-paper hover:text-ink"
                aria-label={t("chat.react")}
                aria-expanded={picking}
                onClick={(e) => {
                  setBelow(e.currentTarget.getBoundingClientRect().top < 160);
                  setPicking(!picking);
                }}
              >
                <IconSmile size={16} />
              </button>
              <button type="button" className="hidden rounded-full p-1.5 text-muted hover:bg-paper hover:text-ink sm:block" aria-label={t("chat.reply")} onClick={p.onReply}>
                <IconReply size={16} />
              </button>
            </>
          )}
          <MenuButton iconOnly variant="ghost" label={t("chat.messageActions")} icon={<IconMore size={16} />} items={menu} />
        </div>
        {picking && (
          <div className={`absolute z-20 ${below ? "top-full mt-1" : "bottom-full mb-1"} ${mine ? "right-0" : "left-0"}`}>
            <ReactionPicker
              toggle={smile}
              onClose={closePicker}
              onPick={(e) => {
                setPicking(false);
                p.onReact(e);
              }}
            />
          </div>
        )}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ Поиск и файлы

function SidePanel({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  const { t } = useTranslation();
  return (
    <aside className="absolute inset-0 z-30 flex flex-col border-l border-line bg-paper sm:static sm:w-80 sm:shrink-0" aria-label={title}>
      <div className="flex items-center justify-between gap-2 border-b border-line px-3 py-2">
        <h3 className="font-semibold">{title}</h3>
        <button type="button" className="rounded-lg p-1 text-muted hover:bg-hover" aria-label={t("common.close")} onClick={onClose}>
          <IconX size={18} />
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">{children}</div>
    </aside>
  );
}

function SearchPanel({ chat, onClose, onJump }: { chat: string; onClose: () => void; onJump: (id: string) => void }) {
  const { t, i18n } = useTranslation();
  const [q, setQ] = useState("");
  const [term, setTerm] = useState("");
  useEffect(() => {
    const timer = window.setTimeout(() => setTerm(q.trim()), 300);
    return () => window.clearTimeout(timer);
  }, [q]);
  const found = useQuery({
    queryKey: ["chat-search", chat, term],
    queryFn: () => ok<ChatMessage[]>(api.GET("/api/chat/{id}/search", { params: { path: { id: chat }, query: { q: term } } })),
    enabled: term.length >= 2,
  });
  return (
    <SidePanel title={t("chat.searchInChat")} onClose={onClose}>
      <div className="p-3">
        <Input type="search" autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("chat.searchPlaceholder")} aria-label={t("chat.searchInChat")} />
      </div>
      {term.length < 2 ? (
        <p className="px-3 text-sm text-muted">{t("chat.searchHint")}</p>
      ) : found.error ? (
        <ErrorBox error={found.error} />
      ) : found.isPending ? (
        <Spinner variant="list" />
      ) : found.data.length === 0 ? (
        <p className="px-3 text-sm text-muted">{t("chat.nothingFound")}</p>
      ) : (
        <>
          <p className="px-3 pb-1 text-xs text-muted">{t("chat.foundN", { count: found.data.length })}</p>
          <ul className="divide-y divide-line">
            {found.data.map((m) => (
              <li key={m.id}>
                <button type="button" className="w-full px-3 py-2 text-left hover:bg-hover" onClick={() => onJump(m.id)}>
                  <span className="flex items-baseline justify-between gap-2">
                    <span className="truncate text-sm font-medium">{m.author_name}</span>
                    <span className="shrink-0 text-[11px] text-muted">{formatDate(m.created_at, i18n.language)}</span>
                  </span>
                  <Highlighted text={m.body} term={term} />
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
    </SidePanel>
  );
}

/** Фрагмент вокруг первого совпадения, совпадение подсвечено. */
function Highlighted({ text, term }: { text: string; term: string }) {
  const i = text.toLowerCase().indexOf(term.toLowerCase());
  if (i < 0) return <span className="line-clamp-2 text-sm text-muted">{text}</span>;
  const from = Math.max(0, i - 40);
  return (
    <span className="line-clamp-2 text-sm text-muted">
      {from > 0 && "…"}
      {text.slice(from, i)}
      <mark className="rounded bg-accent/25 px-0.5 text-ink">{text.slice(i, i + term.length)}</mark>
      {text.slice(i + term.length, i + term.length + 120)}
    </span>
  );
}

function FilesPanel({ chat, onClose, onJump }: { chat: string; onClose: () => void; onJump: (id: string) => void }) {
  const { t, i18n } = useTranslation();
  const [open, setOpen] = useState<number | null>(null);
  const q = useQuery({ queryKey: ["chat-files", chat], queryFn: () => ok<ChatSharedFile[]>(api.GET("/api/chat/{id}/files", { params: { path: { id: chat } } })) });
  const images = (q.data ?? []).filter((x) => x.file.image && !x.file.expired);
  const docs = (q.data ?? []).filter((x) => !x.file.image || x.file.expired);
  return (
    <SidePanel title={t("chat.sharedFiles")} onClose={onClose}>
      {q.error ? (
        <ErrorBox error={q.error} />
      ) : q.isPending ? (
        <Spinner variant="list" />
      ) : q.data.length === 0 ? (
        <p className="p-3 text-sm text-muted">{t("chat.noSharedFiles")}</p>
      ) : (
        <div className="space-y-4 p-3">
          {images.length > 0 && (
            <section>
              <h4 className="mb-1.5 text-xs font-semibold text-muted uppercase">{t("chat.photos", { count: images.length })}</h4>
              <div className="grid grid-cols-3 gap-1">
                {images.map((x, i) => (
                  <button key={x.file.id} type="button" className="aspect-square overflow-hidden rounded-md bg-sunken" onClick={() => setOpen(i)} aria-label={x.file.name}>
                    <img src={chatFileUrl(x.file.id)} alt="" loading="lazy" className="size-full object-cover" />
                  </button>
                ))}
              </div>
            </section>
          )}
          {docs.length > 0 && (
            <section>
              <h4 className="mb-1.5 text-xs font-semibold text-muted uppercase">{t("chat.documents", { count: docs.length })}</h4>
              <ul className="space-y-1">
                {docs.map((x) => (
                  <li key={x.file.id} className="flex items-center gap-2 rounded-lg px-1 py-1 hover:bg-hover">
                    <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-accentsoft text-accent-ink">
                      <IconFileText size={18} />
                    </span>
                    <span className="min-w-0 flex-1">
                      {x.file.expired ? (
                        <span className="block truncate text-sm text-muted line-through">{x.file.name}</span>
                      ) : (
                        <a href={chatFileUrl(x.file.id)} download={x.file.name} className="block truncate text-sm font-medium hover:underline">
                          {x.file.name}
                        </a>
                      )}
                      <span className="block truncate text-[11px] text-muted">
                        {x.file.expired ? t("chat.expired") : formatSize(x.file.size)} · {x.author_name} · {formatDate(x.created_at, i18n.language)}
                      </span>
                    </span>
                    <button type="button" className="shrink-0 rounded p-1 text-muted hover:bg-paper hover:text-ink" aria-label={t("chat.showInChat")} title={t("chat.showInChat")} onClick={() => onJump(x.message_id)}>
                      <IconArrowLeft size={14} />
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          )}
        </div>
      )}
      {open !== null && (
        <Lightbox
          items={images.map((x) => ({ src: chatFileUrl(x.file.id), name: x.file.name }))}
          index={open}
          onIndex={setOpen}
          onClose={() => setOpen(null)}
        />
      )}
    </SidePanel>
  );
}
