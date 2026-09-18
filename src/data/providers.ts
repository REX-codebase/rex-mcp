export type ProviderProtocol = "gemini" | "anthropic" | "openai-compatible";
export type ProviderStatus = "not-configured" | "draft" | "needs-runtime" | "unsupported";

export type ProviderPreset = {
  id: string;
  name: string;
  protocol: ProviderProtocol;
  summary: string;
  baseUrl?: string;
  baseUrlLocked?: boolean;
  modelDiscovery: "native" | "openai-models" | "manual";
  examples?: string;
};

export type ProviderDraft = {
  presetId: string;
  displayName: string;
  protocol: ProviderProtocol;
  baseUrl: string;
  modelId: string;
  keyReference: "desktop-keychain";
  status: ProviderStatus;
};

export const PROVIDER_PRESETS: ProviderPreset[] = [
  {
    id: "gemini",
    name: "Google Gemini",
    protocol: "gemini",
    summary: "Native Gemini generateContent and model listing.",
    modelDiscovery: "native",
  },
  {
    id: "anthropic",
    name: "Anthropic",
    protocol: "anthropic",
    summary: "Native Claude Messages API and model listing.",
    modelDiscovery: "native",
  },
  {
    id: "deepseek",
    name: "DeepSeek",
    protocol: "openai-compatible",
    summary: "Preset for DeepSeek's OpenAI-compatible API.",
    baseUrl: "https://api.deepseek.com",
    modelDiscovery: "openai-models",
  },
  {
    id: "kimi",
    name: "Kimi",
    protocol: "openai-compatible",
    summary: "Preset for Moonshot AI's OpenAI-compatible API.",
    baseUrl: "https://api.moonshot.ai/v1",
    modelDiscovery: "openai-models",
  },
  {
    id: "glm",
    name: "GLM",
    protocol: "openai-compatible",
    summary: "Preset for Zhipu AI's OpenAI-compatible API.",
    baseUrl: "https://open.bigmodel.cn/api/paas/v4",
    modelDiscovery: "openai-models",
  },
  {
    id: "local",
    name: "Local server",
    protocol: "openai-compatible",
    summary: "Ollama, LM Studio, vLLM, or another local compatible server.",
    baseUrl: "http://127.0.0.1:11434/v1",
    modelDiscovery: "openai-models",
  },
  {
    id: "custom",
    name: "Custom endpoint",
    protocol: "openai-compatible",
    summary: "Any endpoint that implements the calls REX needs.",
    baseUrl: "",
    modelDiscovery: "manual",
    examples: "OpenRouter, Azure-style gateways, proxies, and self-hosted runtimes",
  },
];

export const protocolLabel: Record<ProviderProtocol, string> = {
  gemini: "Native Gemini",
  anthropic: "Native Anthropic",
  "openai-compatible": "OpenAI-compatible",
};
