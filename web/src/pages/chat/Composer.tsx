// Поле ввода: растёт вместе с текстом, Enter отправляет, Shift+Enter — новая строка, ↑ в пустом поле —
// правка последнего своего сообщения, Esc — отмена ответа или правки. Вложения — скрепкой, вставкой
// или перетаскиванием на переписку; эмодзи — кнопкой. Набор текста сообщается собеседникам («печатает…»).
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "../../toast";
import { api, ok, type ChatKlipyGif, type ChatMessage } from "../../api/client";
import { IconFileText, IconPaperclip, IconReply, IconSend, IconSmile, IconX } from "../../icons";
import { ErrorBox } from "../../ui";
import { formatSize } from "../helpdesk/helpdeskUtil";
import { EmojiPanel, Quote } from "./ChatParts";
import { dropPending, MAX_FILES, quoteOf, sendTyping, usePendingFiles } from "./chatUtil";

/** Неотправленный текст по чатам: не теряется, когда открывают правку другого сообщения или другой чат. */
const drafts = new Map<string, string>();
const MAX_HEIGHT = 200;

type Props = {
  chat: string;
  editing: ChatMessage | null;
  reply: ChatMessage | null;
  onAddFiles: (files: File[]) => void;
  onEditLast: () => void;
  onJump: (id: string) => void;
  onDone: () => void;
  onCancelReply: () => void;
};

export function Composer({ chat, editing, reply, onAddFiles, onEditLast, onJump, onDone, onCancelReply }: Props) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [text, setTextState] = useState(editing ? editing.body : (drafts.get(chat) ?? ""));
  const [emoji, setEmoji] = useState(false);
  const closeEmoji = useCallback(() => setEmoji(false), []);
  const setText = (v: string) => {
    setTextState(v);
    if (!editing) {
      if (v) drafts.set(chat, v);
      else drafts.delete(chat);
    }
  };
  const pendingFiles = usePendingFiles(chat);
  const files = editing ? [] : pendingFiles.files;
  const uploading = editing ? 0 : pendingFiles.uploading;
  const picker = useRef<HTMLInputElement>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  const smile = useRef<HTMLButtonElement>(null);
  // На телефоне поле не забирает фокус при открытии чата (клавиатура закрыла бы переписку) —
  // только при ответе и правке.
  useEffect(() => {
    if (editing || reply || window.matchMedia("(pointer: fine)").matches) box.current?.focus();
  }, [editing, reply]);
  // Высота по тексту, до MAX_HEIGHT — дальше прокрутка внутри поля.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, MAX_HEIGHT)}px`;
  }, [text]);
  const send = useMutation({
    // Отправляется то, что было в поле в момент нажатия: добавленное во время отправки остаётся в черновике.
    mutationFn: ({ body, ids, replyTo }: { body: string; ids: string[]; replyTo?: string }) =>
      editing
        ? ok(api.PUT("/api/chat/{id}/messages/{message}", { params: { path: { id: chat, message: editing.id } }, body: { body } }))
        : ok(api.POST("/api/chat/{id}/messages", { params: { path: { id: chat } }, body: { body, files: ids, reply_to: replyTo } })),
    onSuccess: (_, sent) => {
      if (editing || (drafts.get(chat) ?? "") === sent.body) setText("");
      if (!editing) dropPending(chat, sent.ids);
      onDone();
      void qc.invalidateQueries({ queryKey: ["chat-messages", chat] });
      void qc.invalidateQueries({ queryKey: ["chats"] });
      if (sent.ids.length) void qc.invalidateQueries({ queryKey: ["chat-files", chat] });
    },
  });
  const sendGif = useMutation({
    mutationFn: (gif: string) => ok(api.POST("/api/chat/{id}/gifs/{gif}", { params: { path: { id: chat, gif } }, body: { reply_to: reply?.id } })),
    onSuccess: () => {
      setEmoji(false);
      onDone();
      void qc.invalidateQueries({ queryKey: ["chat-messages", chat] });
      void qc.invalidateQueries({ queryKey: ["chats"] });
      void qc.invalidateQueries({ queryKey: ["chat-files", chat] });
    },
    onError: (e) => toast((e instanceof Error && e.message) || t("common.error"), "bad"),
  });
  const sendKlipy = useMutation({
    mutationFn: (gif: ChatKlipyGif) => ok(api.POST("/api/chat/{id}/klipy", { params: { path: { id: chat } }, body: { ...gif, reply_to: reply?.id } })),
    onSuccess: () => {
      setEmoji(false);
      onDone();
      void qc.invalidateQueries({ queryKey: ["chat-messages", chat] });
      void qc.invalidateQueries({ queryKey: ["chats"] });
      void qc.invalidateQueries({ queryKey: ["chat-files", chat] });
    },
    onError: (e) => toast((e instanceof Error && e.message) || t("common.error"), "bad"),
  });
  const ready = (text.trim() !== "" || files.length > 0) && uploading === 0 && !send.isPending;
  const submit = () => {
    if (ready) send.mutate({ body: text, ids: files.map((f) => f.id), replyTo: reply?.id });
  };
  const insert = (s: string) => {
    const el = box.current;
    const start = el?.selectionStart ?? text.length;
    const end = el?.selectionEnd ?? text.length;
    setText(text.slice(0, start) + s + text.slice(end));
    requestAnimationFrame(() => {
      el?.focus();
      el?.setSelectionRange(start + s.length, start + s.length);
    });
  };
  const iconButton = "flex size-10 shrink-0 items-center justify-center rounded-full text-muted transition-colors hover:text-accent disabled:opacity-40";
  const error = send.error;
  return (
    <div className="px-2 pt-1 pb-[max(0.5rem,env(safe-area-inset-bottom))] sm:px-4 sm:pb-3">
      <div className="mx-auto flex max-w-3xl items-end gap-2">
        <div className="relative min-w-0 flex-1 rounded-[1.4rem] bg-paper shadow-[0_1px_2px_rgb(16_24_40/0.12)] ring-1 ring-line/60 focus-within:ring-accent/50">
          {editing && (
            <div className="flex items-center gap-2 border-b border-line px-3 py-1.5">
              <span className="text-lg text-accent" aria-hidden="true">
                ✎
              </span>
              <div className="min-w-0 flex-1">
                <Quote q={quoteOf(editing)} label={t("chat.editing")} onClick={() => onJump(editing.id)} onClear={onDone} />
              </div>
            </div>
          )}
          {!editing && reply && (
            <div className="flex items-center gap-2 border-b border-line px-3 py-1.5">
              <IconReply size={18} className="shrink-0 text-accent" />
              <div className="min-w-0 flex-1">
                <Quote q={quoteOf(reply)} label={t("chat.replyTo", { name: reply.author_name })} onClick={() => onJump(reply.id)} onClear={onCancelReply} />
              </div>
            </div>
          )}
          {error && (
            <div className="px-2 pt-2">
              <ErrorBox error={error} />
            </div>
          )}
          {!editing && (files.length > 0 || uploading > 0 || pendingFiles.error) && (
            <div className="flex flex-wrap items-center gap-1.5 border-b border-line px-2 py-2 text-xs">
              {files.map((f) => (
                <span key={f.id} className="inline-flex max-w-full items-center gap-1.5 rounded-xl bg-sunken py-1 pr-1 pl-1">
                  {f.image ? (
                    <img src={`/api/chat/files/${f.id}`} alt="" className="size-9 rounded-lg object-cover" />
                  ) : (
                    <span className="flex size-9 items-center justify-center rounded-lg bg-accent text-paper">
                      <IconFileText size={16} />
                    </span>
                  )}
                  <span className="min-w-0">
                    <span className="block max-w-40 truncate font-medium">{f.name}</span>
                    <span className="text-muted tabular-nums">{formatSize(f.size)}</span>
                  </span>
                  <button type="button" className="rounded-full p-1 text-muted hover:bg-hover" aria-label={t("chat.removeFile", { name: f.name })} onClick={() => dropPending(chat, [f.id])}>
                    <IconX size={12} />
                  </button>
                </span>
              ))}
              {uploading > 0 && (
                <span role="status" className="inline-flex items-center gap-1.5 text-muted">
                  <span className="size-3 animate-spin rounded-full border-2 border-accent border-t-transparent" aria-hidden="true" />
                  {t("chat.uploading")}
                </span>
              )}
              {pendingFiles.error && (
                <span role="alert" className="text-bad">
                  {pendingFiles.error}
                </span>
              )}
            </div>
          )}
          <div className="flex items-end px-1">
            <button ref={smile} type="button" className={iconButton} aria-label={t("chat.emoji")} aria-expanded={emoji} onClick={() => setEmoji(!emoji)}>
              <IconSmile size={22} />
            </button>
            <textarea
              ref={box}
              rows={1}
              maxLength={4000}
              value={text}
              aria-label={t("chat.message")}
              title={t("chat.placeholderHint")}
              placeholder={t("chat.placeholder")}
              className="max-h-[200px] min-h-11 w-full resize-none bg-transparent px-1 py-3 text-[15px] leading-5 outline-none placeholder:text-muted focus-visible:outline-none"
              onChange={(e) => {
                setText(e.target.value);
                if (!editing && e.target.value.trim()) sendTyping(chat);
              }}
              onPaste={(e) => {
                // Из Word или браузера вместе с текстом копируется и картинка-превью: тогда вставляется текст.
                const pasted = [...e.clipboardData.files];
                if (pasted.length > 0 && !editing && !e.clipboardData.getData("text/plain")) {
                  e.preventDefault();
                  onAddFiles(pasted);
                }
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                  e.preventDefault();
                  submit();
                } else if (e.key === "Escape") {
                  if (editing) onDone();
                  else if (reply) onCancelReply();
                } else if (e.key === "ArrowUp" && !text && !editing) {
                  e.preventDefault();
                  onEditLast();
                }
              }}
            />
            {!editing && (
              <>
                <button type="button" className={iconButton} aria-label={t("chat.attach")} title={t("chat.attachHint")} disabled={files.length + uploading >= MAX_FILES} onClick={() => picker.current?.click()}>
                  <IconPaperclip size={21} />
                </button>
                <input
                  ref={picker}
                  type="file"
                  multiple
                  className="hidden"
                  onChange={(e) => {
                    onAddFiles([...(e.target.files ?? [])]);
                    e.target.value = "";
                  }}
                />
              </>
            )}
          </div>
          {emoji && (
            <EmojiPanel
              toggle={smile}
              onClose={closeEmoji}
              onPick={(e) => insert(e)}
              onGif={editing ? undefined : (g) => !sendGif.isPending && sendGif.mutate(g)}
              onKlipy={editing ? undefined : (g) => !sendKlipy.isPending && sendKlipy.mutate(g)}
              gifBusy={sendGif.isPending || sendKlipy.isPending}
            />
          )}
        </div>
        <button
          type="button"
          className="flex size-12 shrink-0 items-center justify-center rounded-full bg-accent text-paper shadow-md transition-transform hover:scale-105 active:scale-95 disabled:scale-100 disabled:opacity-50 disabled:shadow-none"
          disabled={!ready}
          aria-label={editing ? t("common.save") : t("chat.send")}
          title={editing ? t("common.save") : t("chat.send")}
          onClick={submit}
        >
          {editing ? (
            <span className="text-xl" aria-hidden="true">
              ✓
            </span>
          ) : (
            <IconSend size={20} />
          )}
        </button>
      </div>
    </div>
  );
}
