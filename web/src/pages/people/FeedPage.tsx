// Лента: записи со стен всех коллег, новые сверху. Обновляется сама по событию сервера `wall`.
import { useInfiniteQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { api, ok, type WallPage } from "../../api/client";
import { useMe } from "../../session";
import { Button, Empty, ErrorBox, Spinner } from "../../ui";
import { useMyCard } from "./profileHooks";
import { PostCard, WallComposer } from "./WallParts";

export function FeedPage() {
  const { t } = useTranslation();
  const me = useMe().data;
  const card = useMyCard().data;
  const q = useInfiniteQuery({
    queryKey: ["feed"],
    queryFn: ({ pageParam }) => ok<WallPage>(api.GET("/api/feed", { params: { query: { before: pageParam } } })),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => (last.more ? last.posts.at(-1)?.id : undefined),
    // События приходят сразу; раз в минуту — на случай, если поток событий прервался.
    refetchInterval: 60_000,
  });
  const posts = q.data?.pages.flatMap((p) => p.posts) ?? [];
  return (
    <div className="mx-auto max-w-3xl space-y-3">
      <div className="px-1">
        <h1 className="text-xl font-semibold">{t("people.feed.title")}</h1>
        <p className="text-sm text-muted">{t("people.feed.hint")}</p>
      </div>
      {me && card && card.wall_mode !== "off" && <WallComposer wall={me.user_id} placeholder={t("people.wall.placeholderMine")} />}
      {q.isPending && <Spinner variant="list" />}
      {q.error && <ErrorBox error={q.error} onRetry={() => void q.refetch()} />}
      {q.data && posts.length === 0 && <Empty>{t("people.feed.empty")}</Empty>}
      <div aria-live="polite" className="space-y-3">
        {posts.map((post) => (
          <PostCard key={post.id} post={post} canReply showWall />
        ))}
      </div>
      {q.hasNextPage && (
        <div className="flex justify-center">
          <Button variant="ghost" disabled={q.isFetchingNextPage} onClick={() => void q.fetchNextPage()}>
            {t("people.wall.more")}
          </Button>
        </div>
      )}
    </div>
  );
}
