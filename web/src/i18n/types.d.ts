// Ключи переводов проверяет компилятор: t("status.titel") не соберётся.
import "i18next";
import type ru from "./ru.json";

declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "translation";
    resources: { translation: typeof ru };
  }
}
