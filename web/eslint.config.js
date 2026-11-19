import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import globals from "globals";
import tseslint from "typescript-eslint";

export default tseslint.config(
  // schema.ts генерируется из OpenAPI (`npm run gen:api`) — не правится руками.
  { ignores: ["dist", "src/api/schema.ts"] },
  {
    files: ["**/*.{ts,tsx}"],
    extends: [js.configs.recommended, ...tseslint.configs.strict],
    languageOptions: { ecmaVersion: 2022, globals: globals.browser },
    plugins: { "react-hooks": reactHooks, "react-refresh": reactRefresh },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": ["warn", { allowConstantExport: true }],
      // Строки интерфейса — только через i18n (план 16: ключи с первого дня).
      "no-restricted-syntax": [
        "error",
        {
          selector: "JSXText[value=/[А-Яа-яЁё]/]",
          message: "Текст интерфейса — через t('ключ'), не строкой в разметке",
        },
        // Без вставки HTML: XSS из текста заявок и писем (план 20).
        {
          selector: "JSXAttribute[name.name='dangerouslySetInnerHTML']",
          message: "Без dangerouslySetInnerHTML — XSS; HTML писем показывается только после очистки на сервере",
        },
      ],
      "no-restricted-properties": [
        "error",
        { property: "innerHTML", message: "Без innerHTML — XSS" },
        { property: "outerHTML", message: "Без outerHTML — XSS" },
      ],
    },
  },
);
