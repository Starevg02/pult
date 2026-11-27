// Поле ввода: растёт вместе с текстом, Enter отправляет, Shift+Enter — новая строка, ↑ в пустом поле —
// правка последнего своего сообщения, Esc — отмена ответа или правки. Вложения — скрепкой, вставкой
// или перетаскиванием на переписку; эмодзи — кнопкой. Набор текста сообщается собеседникам («печатает…»).
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type ChatMessage } from "../../api/client";
import { IconFileText, IconPaperclip, IconReply, IconSend, IconSmile, IconX } from "../../icons";
import { ErrorBox } from "../../ui";
import { formatSize } from "../helpdesk/helpdeskUtil";
import { EmojiPicker, Quote } from "./ChatParts";
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
  const iconButton = "flex size-10 shrink-0 items-center justify-center rounded-full text-muted hover:bg-hover hover:text-ink disabled:opacity-40";
  return (
    <div className="border-t border-line bg-paper px-2 pt-2 pb-2 sm:px-3">
      {editing && (
        <div className="mb-2 flex items-center gap-2 px-1">
          <span className="text-accent" aria-hidden="true">
            ✎
          </span>
          <div className="min-w-0 flex-1">
            <Quote q={quoteOf(editing)} label={t("chat.editing")} onClick={() => onJump(editing.id)} onClear={onDone} />
          </div>
        </div>
      )}
      {!editing && reply && (
        <div className="mb-2 flex items-center gap-2 px-1">
          <IconReply size={18} className="text-accent" />
          <div className="min-w-0 flex-1">
            <Quote q={quoteOf(reply)} label={t("chat.replyTo", { name: reply.author_name })} onClick={() => onJump(reply.id)} onClear={onCancelReply} />
          </div>
        </div>
      )}
      {send.error && <ErrorBox error={send.error} />}
      {!editing && (files.length > 0 || uploading > 0 || pendingFiles.error) && (
        <div className="mb-2 flex flex-wrap items-center gap-1.5 px-1 text-xs">
          {files.map((f) => (
            <span key={f.id} className="inline-flex max-w-full items-center gap-1.5 rounded-xl bg-sunken py-1 pr-1 pl-1">
              {f.image ? (
                <img src={`/api/chat/files/${f.id}`} alt="" className="size-8 rounded-lg object-cover" />
              ) : (
                <span className="flex size-8 items-center justify-center rounded-lg bg-accentsoft text-accent-ink">
                  <IconFileText size={16} />
                </span>
              )}
              <span className="min-w-0">
                <span className="block max-w-40 truncate">{f.name}</span>
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
      <div className="flex items-end gap-1">
        {!editing && (
          <>
            <button type="button" className={iconButton} aria-label={t("chat.attach")} title={t("chat.attachHint")} disabled={files.length + uploading >= MAX_FILES} onClick={() => picker.current?.click()}>
              <IconPaperclip size={20} />
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
        <div className="relative flex min-w-0 flex-1 items-end rounded-3xl border border-control bg-sunken/60 focus-within:border-accent focus-within:bg-paper">
          <textarea
            ref={box}
            rows={1}
            maxLength={4000}
            value={text}
            aria-label={t("chat.message")}
            placeholder={t("chat.placeholder")}
            className="max-h-[200px] min-h-10 w-full resize-none bg-transparent py-2.5 pr-1 pl-4 text-[15px] leading-5 outline-none focus-visible:outline-none"
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
          <button ref={smile} type="button" className="mb-0.5 flex size-9 shrink-0 items-center justify-center rounded-full text-muted hover:text-ink" aria-label={t("chat.emoji")} aria-expanded={emoji} onClick={() => setEmoji(!emoji)}>
            <IconSmile size={20} />
          </button>
          {emoji && <EmojiPicker toggle={smile} onClose={closeEmoji} onPick={(e) => insert(e)} />}
        </div>
        <button
          type="button"
          className="flex size-10 shrink-0 items-center justify-center rounded-full bg-accent text-paper shadow-sm transition-transform hover:scale-105 disabled:scale-100 disabled:opacity-40"
          disabled={!ready}
          aria-label={editing ? t("common.save") : t("chat.send")}
          title={editing ? t("common.save") : t("chat.send")}
          onClick={submit}
        >
          {editing ? <span aria-hidden="true">✓</span> : <IconSend size={18} />}
        </button>
      </div>
    </div>
  );
}
