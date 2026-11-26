// Записи стены: поле ввода, запись с ответами, «нравится». Общие для страницы сотрудника и ленты.
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Likes, type WallPost } from "../../api/client";
import { formatDate } from "../../format";
import { IconHeart, IconTrash } from "../../icons";
import { Button, Card } from "../../ui";
import { UserAvatar } from "./presence";
import { refreshWalls } from "./profileHooks";

export function WallComposer({ wall, parent, placeholder, onDone }: { wall: string; parent?: string; placeholder: string; onDone?: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [body, setBody] = useState("");
  const send = useMutation({
    mutationFn: () => ok<WallPost>(api.POST("/api/users/{id}/wall", { params: { path: { id: wall } }, body: { body, parent_id: parent ?? null } })),
    onSuccess: () => {
      setBody("");
      refreshWalls(qc);
      onDone?.();
    },
  });
  const submit = (e: { preventDefault(): void }) => {
    e.preventDefault();
    if (body.trim() && !send.isPending) send.mutate();
  };
  return (
    <form onSubmit={submit} className={parent ? "mt-2" : ""}>
      <Card className={parent ? "border-0 bg-transparent p-0 shadow-none" : "p-3"}>
        <textarea
          value={body}
          onChange={(e) => setBody(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) submit(e);
          }}
          rows={parent ? 2 : 3}
          maxLength={2000}
          aria-label={placeholder}
          placeholder={placeholder}
          className="w-full resize-y rounded-lg border border-control bg-paper px-3 py-2 text-sm"
        />
        {send.error && <p className="mt-1 text-sm text-bad">{send.error.message}</p>}
        <div className="mt-2 flex items-center justify-between gap-2">
          <span className="text-xs text-muted">{body.length > 1800 ? `${body.length} / 2000` : ""}</span>
          <Button type="submit" variant="primary" disabled={send.isPending || !body.trim()}>
            {parent ? t("people.wall.reply") : t("people.wall.post")}
          </Button>
        </div>
      </Card>
    </form>
  );
}

/** Запись с ответами. `showWall` — в ленте: чья это стена, если пишут не на своей. */
export function PostCard({ post, canReply, showWall }: { post: WallPost; canReply: boolean; showWall?: boolean }) {
  const wall = post.wall.user_id;
  const { t } = useTranslation();
  const [replying, setReplying] = useState(false);
  return (
    <Card className="p-3 sm:p-4">
      <PostBody post={post} showWall={showWall} />
      {!post.deleted && (
        <div className="mt-1 ml-11 flex items-center gap-1">
          <LikeButton post={post} />
          {canReply && (
            <button type="button" className="min-h-8 rounded-lg px-2 text-xs text-muted hover:bg-hover hover:text-ink" onClick={() => setReplying(!replying)} aria-expanded={replying}>
              {t("people.wall.reply")}
            </button>
          )}
        </div>
      )}
      {(post.replies.length > 0 || replying) && (
        <div className="mt-2 ml-5 space-y-3 border-l-2 border-line pl-4 sm:ml-11">
          {post.replies.map((r) => (
            <div key={r.id}>
              <PostBody post={r} small />
              <div className="ml-9">
                <LikeButton post={r} />
              </div>
            </div>
          ))}
          {replying && <WallComposer wall={wall} parent={post.id} placeholder={t("people.wall.replyPlaceholder")} onDone={() => setReplying(false)} />}
        </div>
      )}
    </Card>
  );
}

function PostBody({ post, small, showWall }: { post: WallPost; small?: boolean; showWall?: boolean }) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const del = useMutation({
    mutationFn: () => ok(api.DELETE("/api/wall/{id}", { params: { path: { id: post.id } } })),
    onSuccess: () => refreshWalls(qc),
  });
  const elsewhere = showWall && post.wall.user_id !== post.author.user_id;
  return (
    <div className="flex gap-3">
      <Link to="/people/$id" params={{ id: post.author.user_id }} className="shrink-0" aria-hidden="true" tabIndex={-1}>
        <UserAvatar name={post.author.name} userId={post.author.user_id} avatar={post.author.avatar} size={small ? "xs" : "sm"} />
      </Link>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-baseline gap-x-2">
          <Link to="/people/$id" params={{ id: post.author.user_id }} className="text-sm font-medium hover:underline">
            {post.author.name}
          </Link>
          {elsewhere && (
            <span className="text-sm text-muted">
              {"→ "}
              <Link to="/people/$id" params={{ id: post.wall.user_id }} className="hover:underline" aria-label={t("people.feed.onWall", { name: post.wall.name })}>
                {post.wall.name}
              </Link>
            </span>
          )}
          <span className="text-xs text-muted">{formatDate(post.created_at, i18n.language)}</span>
          {post.can_delete && (
            <button
              type="button"
              aria-label={t("common.delete")}
              title={t("common.delete")}
              disabled={del.isPending}
              className="ml-auto inline-flex h-7 w-7 items-center justify-center rounded-md text-muted hover:bg-hover hover:text-bad"
              onClick={() => window.confirm(t("people.wall.deleteConfirm")) && del.mutate()}
            >
              <IconTrash size={14} />
            </button>
          )}
        </div>
        {post.deleted ? (
          <p className="text-sm text-muted italic">{t("people.wall.deleted")}</p>
        ) : (
          <p className="text-sm break-words whitespace-pre-wrap">{post.body}</p>
        )}
      </div>
    </div>
  );
}

function LikeButton({ post }: { post: WallPost }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const like = useMutation({
    mutationFn: () =>
      ok<Likes>(post.liked ? api.DELETE("/api/wall/{id}/like", { params: { path: { id: post.id } } }) : api.PUT("/api/wall/{id}/like", { params: { path: { id: post.id } } })),
    onSuccess: () => refreshWalls(qc),
  });
  return (
    <button
      type="button"
      aria-pressed={post.liked}
      aria-label={t("people.wall.likes", { count: post.likes })}
      title={t("people.wall.like")}
      disabled={like.isPending}
      className={`inline-flex min-h-8 items-center gap-1 rounded-lg px-2 text-xs hover:bg-hover ${post.liked ? "text-bad" : "text-muted"}`}
      onClick={() => like.mutate()}
    >
      <IconHeart size={14} fill={post.liked ? "currentColor" : "none"} />
      {post.likes > 0 && <span className="tabular-nums">{post.likes}</span>}
    </button>
  );
}
