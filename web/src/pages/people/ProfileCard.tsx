// «Профиль для коллег» в моём профиле: аватар, «о себе», кто пишет на моей стене.
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Me, type MyCard, type WallMode } from "../../api/client";
import { IconCamera } from "../../icons";
import { toast } from "../../toast";
import { Button, Card, ErrorBox, Field, Select, Spinner } from "../../ui";
import { UserAvatar } from "./presence";
import { useMyCard } from "./profileHooks";

/** Сторона квадрата аватара: в списках он мельче, на странице сотрудника — 96 точек (×2 для чётких экранов). */
const SIDE = 256;
const MAX_BYTES = 1024 * 1024;
const WALL_MODES: WallMode[] = ["everyone", "me", "off"];

/** Картинку — в квадрат по центру и уменьшить; WebP, если браузер умеет, иначе JPEG. */
async function squareImage(file: File): Promise<Blob> {
  const url = URL.createObjectURL(file);
  try {
    const img = await new Promise<HTMLImageElement>((resolve, reject) => {
      const i = new Image();
      i.onload = () => resolve(i);
      i.onerror = () => reject(new Error("decode"));
      i.src = url;
    });
    const side = Math.min(img.naturalWidth, img.naturalHeight);
    if (!side) throw new Error("decode");
    const canvas = document.createElement("canvas");
    canvas.width = SIDE;
    canvas.height = SIDE;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("decode");
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(img, (img.naturalWidth - side) / 2, (img.naturalHeight - side) / 2, side, side, 0, 0, SIDE, SIDE);
    const blob = (type: string) => new Promise<Blob | null>((r) => canvas.toBlob(r, type, 0.85));
    const webp = await blob("image/webp");
    const out = webp && webp.type === "image/webp" ? webp : await blob("image/jpeg");
    if (!out) throw new Error("decode");
    return out;
  } finally {
    URL.revokeObjectURL(url);
  }
}

/** GIF по сигнатуре файла, а не по расширению или типу из браузера. */
async function isGif(f: File): Promise<boolean> {
  const head = new TextDecoder("latin1").decode(await f.slice(0, 6).arrayBuffer());
  return head === "GIF87a" || head === "GIF89a";
}

function toBase64(b: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).replace(/^data:[^,]*,/, ""));
    r.onerror = () => reject(new Error("read"));
    r.readAsDataURL(b);
  });
}

export function ColleagueProfile({ me }: { me: Me }) {
  const { t } = useTranslation();
  const titleId = useId();
  const card = useMyCard();
  return (
    <section id="card" aria-labelledby={titleId} className="mb-4 scroll-mt-20">
      <Card className="p-4">
        <div className="flex flex-wrap items-baseline justify-between gap-2">
          <h2 id={titleId} className="text-base font-semibold">
            {t("people.profile.title")}
          </h2>
          <Link to="/people/$id" params={{ id: me.user_id }} className="text-sm text-accent hover:underline">
            {t("people.myPage")}
          </Link>
        </div>
        <p className="mt-1 text-sm text-muted">{t("people.profile.hint")}</p>
        <div className="mt-3">
          {card.isPending && <Spinner />}
          {card.error && <ErrorBox error={card.error} onRetry={() => void card.refetch()} />}
          {card.data && <Editor key={`${card.data.about}\u0000${card.data.wall_mode}`} me={me} card={card.data} />}
        </div>
      </Card>
    </section>
  );
}

function Editor({ me, card }: { me: Me; card: MyCard }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const file = useRef<HTMLInputElement>(null);
  const [about, setAbout] = useState(card.about);
  const [wall, setWall] = useState<WallMode>(card.wall_mode);
  const fresh = (c: MyCard) => {
    qc.setQueryData(["my-card"], c);
    void qc.invalidateQueries({ queryKey: ["user-card", me.user_id] });
  };
  const upload = useMutation({
    mutationFn: async (f: File) => {
      let blob: Blob;
      if (await isGif(f)) {
        // GIF — как есть: перерисовка на canvas оставила бы один кадр анимации.
        if (f.size > MAX_BYTES) throw new Error(t("people.profile.gifTooLarge"));
        blob = f;
      } else {
        try {
          blob = await squareImage(f);
        } catch {
          throw new Error(t("people.profile.badImage"));
        }
        if (blob.size > MAX_BYTES) throw new Error(t("people.profile.tooLarge"));
      }
      return ok<MyCard>(api.PUT("/api/me/avatar", { body: { data: await toBase64(blob) } }));
    },
    onSuccess: (c) => {
      fresh(c);
      toast(t("people.profile.avatarSaved"));
    },
  });
  const drop = useMutation({
    mutationFn: () => ok(api.DELETE("/api/me/avatar")),
    onSuccess: () => fresh({ ...card, avatar: null }),
  });
  const save = useMutation({
    mutationFn: () => ok<MyCard>(api.PUT("/api/me/about", { body: { about, wall_mode: wall } })),
    onSuccess: (c) => {
      fresh(c);
      toast(t("common.saved"));
    },
  });
  const dirty = about !== card.about || wall !== card.wall_mode;
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-4">
        <UserAvatar name={me.display_name} userId={me.user_id} avatar={card.avatar} size="lg" />
        <div className="flex flex-wrap gap-2">
          <input
            ref={file}
            type="file"
            accept="image/jpeg,image/png,image/webp,image/gif"
            className="hidden"
            onChange={(e) => {
              const f = e.target.files?.[0];
              e.target.value = "";
              if (f) upload.mutate(f);
            }}
          />
          <Button onClick={() => file.current?.click()} disabled={upload.isPending}>
            <IconCamera />
            {card.avatar ? t("people.profile.changeAvatar") : t("people.profile.uploadAvatar")}
          </Button>
          {card.avatar && (
            <Button variant="ghost" onClick={() => drop.mutate()} disabled={drop.isPending}>
              {t("people.profile.removeAvatar")}
            </Button>
          )}
        </div>
      </div>
      <p className="text-xs text-muted">{t("people.profile.avatarHint")}</p>
      {(upload.error || drop.error) && <p className="text-sm text-bad">{(upload.error ?? drop.error)?.message}</p>}
      <Field label={t("people.about")} hint={t("people.profile.aboutHint")}>
        <textarea
          value={about}
          onChange={(e) => setAbout(e.target.value)}
          rows={4}
          maxLength={1000}
          className="w-full resize-y rounded-lg border border-control bg-paper px-3 py-2 text-sm"
        />
      </Field>
      <Field label={t("people.wall.mode")}>
        <Select value={wall} onChange={(e) => setWall(e.target.value as WallMode)}>
          {WALL_MODES.map((m) => (
            <option key={m} value={m}>
              {t(`people.wall.modes.${m}`)}
            </option>
          ))}
        </Select>
      </Field>
      {save.error && <p className="text-sm text-bad">{save.error.message}</p>}
      <div className="flex justify-end">
        <Button variant="primary" disabled={!dirty || save.isPending} onClick={() => save.mutate()}>
          {t("common.save")}
        </Button>
      </div>
    </div>
  );
}
