// Каталог AD: коды операций, ключи переводов, нужные разрешения, тона состояний.
import type { OperationKind } from "../../api/client";
import type { Tone } from "./helpdeskUtil";

/** Код операции → ключ перевода (точка в ключе i18next — разделитель). */
export const KIND_KEY: Record<string, string> = {
  "group.add_member": "addMember",
  "user.unlock": "unlock",
  "user.enable": "enable",
  "user.disable": "disable",
  "user.reset_password": "resetPassword",
};

export const KIND_PERM: Record<OperationKind, string> = {
  "group.add_member": "directory.group.add_member",
  "user.unlock": "directory.user.unlock",
  "user.enable": "directory.user.enable",
  "user.disable": "directory.user.enable",
  "user.reset_password": "directory.user.reset_password",
};

export const accessTone: Record<string, Tone> = { new: "muted", pending: "accent", approved: "ok", rejected: "bad", done: "ok" };
