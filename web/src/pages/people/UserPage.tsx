// Страница сотрудника: аватар, присутствие и свой статус, должность и контакты, «о себе», стена.
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type UserCard, type WallPage } from "../../api/client";
import { IconChat, IconMail, IconPhone } from "../../icons";
import { useModule } from "../../session";
import { toast } from "../../toast";
import { Button, Card, Empty, ErrorBox, Spinner } from "../../ui";
import { UserAvatar } from "./presence";
import { PostCard, WallComposer } from "./WallParts";

export function UserPage() {
  const { id } = useParams({ strict: false }) as { id: string };
  const { t } = useTranslation();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const chat = useModule("chat");
  const card = useQuery({ queryKey: ["user-card", id], queryFn: () => ok<UserCard>(api.GET("/api/users/{id}/card", { params: { path: { id } } })) });
  const write = useMutation({
    mutationFn: () => ok<{ id: string }>(api.POST("/api/chat/direct", { body: { user_id: id } })),
    onSuccess: (c) => void navigate({ to: "/chats/$id", params: { id: c.id } }),
  });
  const dropAvatar = useMutation({
    mutationFn: () => ok(api.DELETE("/api/users/{id}/avatar", { params: { path: { id } } })),
    onSuccess: () => {
      toast(t("people.avatarRemoved"));
      void qc.invalidateQueries({ queryKey: ["user-card", id] });
    },
  });
  if (card.isPending) return <Spinner />;
  if (card.error) return <ErrorBox error={card.error} onRetry={() => void card.refetch()} />;
  const c = card.data;
  const p = c.person;
  const subtitle = [p?.position, p?.department, p?.site].filter(Boolean).join(" · ");
  return (
    <div className="mx-auto max-w-3xl space-y-4">
      <Card className="p-4 sm:p-6">
        <div className="flex flex-col items-center gap-4 text-center sm:flex-row sm:items-start sm:text-left">
          <UserAvatar name={c.display_name} userId={c.user_id} avatar={c.avatar} presence={c.presence} size="lg" />
          <div className="min-w-0 flex-1">
            <h1 className="text-xl font-semibold break-words">{c.display_name}</h1>
            <div className="mt-0.5 text-sm text-muted">{t(`people.presence.${c.presence}`)}</div>
            {(c.status_emoji || c.status_text) && (
              <div className="mt-2 inline-flex max-w-full items-center gap-1.5 rounded-full bg-sunken px-3 py-1 text-sm">
                {c.status_emoji && <span aria-hidden="true">{c.status_emoji}</span>}
                <span className="truncate">{c.status_text}</span>
              </div>
            )}
            {subtitle && <div className="mt-2 text-sm">{subtitle}</div>}
            <div className="mt-3 flex flex-wrap justify-center gap-2 sm:justify-start">
              {c.is_me ? (
                <Link to="/profile" hash="card" className="inline-flex min-h-9 items-center rounded-lg border border-control px-3 text-sm hover:bg-hover">
                  {t("people.editProfile")}
                </Link>
              ) : (
                chat && (
                  <Button onClick={() => write.mutate()} disabled={write.isPending}>
                    <IconChat />
                    {t("people.message")}
                  </Button>
                )
              )}
              {c.can_moderate && !c.is_me && c.avatar && (
                <Button variant="ghost" disabled={dropAvatar.isPending} onClick={() => window.confirm(t("people.removeAvatarConfirm")) && dropAvatar.mutate()}>
                  {t("people.removeAvatar")}
                </Button>
              )}
            </div>
            {write.error && <p className="mt-2 text-sm text-bad">{write.error.message}</p>}
          </div>
        </div>
        {p && (p.work_phone || p.internal_ext || p.mobile_phone || p.email || p.room) && (
          <dl className="mt-5 grid gap-x-6 gap-y-2 border-t border-line pt-4 text-sm sm:grid-cols-2">
            {(p.work_phone || p.internal_ext) && (
              <Contact label={t("people.workPhone")}>
                <IconPhone className="text-muted" />
                {[p.work_phone, p.internal_ext && `${t("people.ext")} ${p.internal_ext}`].filter(Boolean).join(", ")}
              </Contact>
            )}
            {p.mobile_phone && (
              <Contact label={t("people.mobile")}>
                <IconPhone className="text-muted" />
                <a href={`tel:${p.mobile_phone}`} className="text-accent hover:underline">
                  {p.mobile_phone}
                </a>
              </Contact>
            )}
            {p.email && (
              <Contact label="Email">
                <IconMail className="text-muted" />
                <a href={`mailto:${p.email}`} className="truncate text-accent hover:underline">
                  {p.email}
                </a>
              </Contact>
            )}
            {p.room && <Contact label={t("people.room")}>{p.room}</Contact>}
          </dl>
        )}
        {c.about && (
          <div className="mt-5 border-t border-line pt-4">
            <h2 className="mb-1 text-sm font-medium text-muted">{t("people.about")}</h2>
            <p className="text-sm break-words whitespace-pre-wrap">{c.about}</p>
          </div>
        )}
      </Card>
      {(c.wall_mode !== "off" || c.is_me) && <Wall key={c.user_id} card={c} />}
    </div>
  );
}

function Contact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="text-xs text-muted">{label}</dt>
      <dd className="flex min-w-0 items-center gap-1.5">{children}</dd>
    </div>
  );
}

function Wall({ card }: { card: UserCard }) {
  const { t } = useTranslation();
  const id = card.user_id;
  const q = useInfiniteQuery({
    queryKey: ["wall", id],
    queryFn: ({ pageParam }) => ok<WallPage>(api.GET("/api/users/{id}/wall", { params: { path: { id }, query: { before: pageParam } } })),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => (last.more ? last.posts.at(-1)?.id : undefined),
  });
  const posts = q.data?.pages.flatMap((p) => p.posts) ?? [];
  return (
    <section aria-labelledby="wall-title" className="space-y-3">
      <h2 id="wall-title" className="px-1 text-base font-semibold">
        {t("people.wall.title")}
      </h2>
      {card.wall_mode === "off" && <p className="px-1 text-sm text-muted">{t("people.wall.offHint")}</p>}
      {card.can_post && <WallComposer wall={id} placeholder={card.is_me ? t("people.wall.placeholderMine") : t("people.wall.placeholder", { name: card.display_name })} />}
      {q.isPending && <Spinner variant="list" />}
      {q.error && <ErrorBox error={q.error} onRetry={() => void q.refetch()} />}
      {q.data && posts.length === 0 && <Empty>{t("people.wall.empty")}</Empty>}
      {posts.map((post) => (
        <PostCard key={post.id} post={post} canReply={card.wall_mode !== "off"} />
      ))}
      {q.hasNextPage && (
        <div className="flex justify-center">
          <Button variant="ghost" disabled={q.isFetchingNextPage} onClick={() => void q.fetchNextPage()}>
            {t("people.wall.more")}
          </Button>
        </div>
      )}
    </section>
  );
}
