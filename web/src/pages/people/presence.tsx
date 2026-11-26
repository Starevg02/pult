// Аватар с точкой присутствия.
import { useTranslation } from "react-i18next";
import type { Presence } from "../../api/client";
import { initials } from "../../format";
import { avatarUrl } from "./profileHooks";

const DOT: Record<Presence, string> = {
  online: "bg-ok",
  away: "bg-away",
  dnd: "bg-bad",
  offline: "bg-line-strong",
};

const SIZES = {
  xs: { box: "h-6 w-6 text-[10px]", dot: "h-2 w-2" },
  sm: { box: "h-8 w-8 text-xs", dot: "h-2.5 w-2.5" },
  md: { box: "h-10 w-10 text-sm", dot: "h-3 w-3" },
  lg: { box: "h-24 w-24 text-2xl", dot: "h-5 w-5 border-[3px]" },
} as const;

export type AvatarSize = keyof typeof SIZES;

/** Точка присутствия; подпись — для экранного диктора и всплывающей подсказки. */
export function PresenceDot({ presence, className = "" }: { presence: Presence; className?: string }) {
  const { t } = useTranslation();
  const label = t(`people.presence.${presence}`);
  return <span role="img" aria-label={label} title={label} className={`inline-block shrink-0 rounded-full ${DOT[presence]} ${className}`} />;
}

/** Аватар: картинка или инициалы, с точкой присутствия в углу. */
export function UserAvatar({
  name,
  userId,
  avatar,
  presence,
  size = "sm",
}: {
  name: string;
  userId?: string | null;
  avatar?: string | null;
  presence?: Presence | null;
  size?: AvatarSize;
}) {
  const s = SIZES[size];
  return (
    <span className="relative inline-flex shrink-0">
      {userId && avatar ? (
        <img src={avatarUrl(userId, avatar)} alt="" className={`${s.box} rounded-full object-cover`} loading="lazy" />
      ) : (
        <span aria-hidden="true" className={`${s.box} flex items-center justify-center rounded-full bg-accentsoft font-semibold text-accent-ink`}>
          {initials(name)}
        </span>
      )}
      {presence && <PresenceDot presence={presence} className={`absolute right-0 bottom-0 border-2 border-paper ${s.dot}`} />}
    </span>
  );
}
