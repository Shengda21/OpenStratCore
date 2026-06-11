export interface LlmConfig {
  provider: "anthropic" | "openai";
  baseUrl: string;
  apiKey: string;
  model: string;
}

export interface OrderList {
  actions: any[];
}

// Ask the configured LLM for this side's orders. `actionListSchema` is a JSON Schema object (the
// tool input shape, i.e. { type:"object", required:["actions"], properties:{actions:{...}} }).
// Returns the parsed { actions: [...] }. Throws Error with a helpful message on any failure.
export async function llmDecide(
  cfg: LlmConfig,
  side: "red" | "blue",
  observation: unknown,
  actionListSchema: object,
  systemPrompt: string,
): Promise<OrderList> {
  const userMsg =
    `You command side "${side}". Decide this tick's orders. Observation JSON:\n` +
    JSON.stringify(observation);

  if (cfg.provider === "anthropic") {
    const response = await postJson(
      `${baseUrl(cfg.baseUrl, "https://api.anthropic.com")}/v1/messages`,
      {
        "x-api-key": cfg.apiKey,
        "anthropic-version": "2023-06-01",
        "anthropic-dangerous-direct-browser-access": "true",
        "content-type": "application/json",
      },
      {
        model: cfg.model,
        max_tokens: 1024,
        system: systemPrompt,
        tools: [
          {
            name: "submit_orders",
            description: "Submit this side's orders for the current decision tick.",
            input_schema: actionListSchema,
          },
        ],
        tool_choice: { type: "tool", name: "submit_orders" },
        messages: [{ role: "user", content: userMsg }],
      },
    );

    const content = asRecord(response).content;
    if (!Array.isArray(content)) {
      throw new Error("Anthropic response did not contain a content array.");
    }

    const toolUse = content.find((block) => asRecord(block).type === "tool_use");
    if (!toolUse) {
      throw new Error("Anthropic response did not include a submit_orders tool_use block.");
    }

    return normalizeOrderList(parseMaybeJson(asRecord(toolUse).input));
  }

  if (cfg.provider === "openai") {
    const response = await postJson(
      `${baseUrl(cfg.baseUrl, "https://api.openai.com/v1")}/chat/completions`,
      {
        Authorization: `Bearer ${cfg.apiKey}`,
        "content-type": "application/json",
      },
      {
        model: cfg.model,
        messages: [
          { role: "system", content: systemPrompt },
          { role: "user", content: userMsg },
        ],
        tools: [
          {
            type: "function",
            function: {
              name: "submit_orders",
              description: "Submit this side's orders for the current decision tick.",
              parameters: actionListSchema,
            },
          },
        ],
        tool_choice: { type: "function", function: { name: "submit_orders" } },
      },
    );

    const message = asRecord(asArray(asRecord(response).choices, "OpenAI choices")[0]).message;
    const messageRecord = asRecord(message);
    const toolCalls = messageRecord.tool_calls;

    if (Array.isArray(toolCalls) && toolCalls.length > 0) {
      const firstCall = asRecord(toolCalls[0]);
      const fn = asRecord(firstCall.function);
      const args = fn.arguments;

      if (typeof args !== "string") {
        throw new Error("OpenAI tool call arguments were missing or not a JSON string.");
      }

      return normalizeOrderList(parseJson(args, "OpenAI tool call arguments"));
    }

    if (typeof messageRecord.content === "string" && messageRecord.content.trim() !== "") {
      return normalizeOrderList(parseJson(messageRecord.content, "OpenAI message content"));
    }

    throw new Error("OpenAI response did not include tool_calls or JSON message content.");
  }

  throw new Error(`Unsupported LLM provider: ${(cfg as { provider?: unknown }).provider}`);
}

function baseUrl(value: string, fallback: string): string {
  return (value || fallback).replace(/\/+$/, "");
}

async function postJson(url: string, headers: Record<string, string>, body: unknown): Promise<unknown> {
  let response: Response;

  try {
    response = await fetch(url, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
    });
  } catch (error) {
    throw new Error(`LLM request failed: ${errorMessage(error)}`);
  }

  const text = await response.text();

  if (!response.ok) {
    throw new Error(`LLM request failed with HTTP ${response.status}: ${text.slice(0, 500)}`);
  }

  return parseJson(text, "LLM response");
}

function parseMaybeJson(value: unknown): unknown {
  return typeof value === "string" ? parseJson(value, "tool input") : value;
}

function parseJson(text: string, label: string): unknown {
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new Error(`Failed to parse ${label} as JSON: ${errorMessage(error)}`);
  }
}

function normalizeOrderList(value: unknown): OrderList {
  if (Array.isArray(value)) {
    return { actions: value };
  }

  const record = asRecord(value);
  if (!Array.isArray(record.actions)) {
    throw new Error("LLM result must be an object with an actions array.");
  }

  return { actions: record.actions };
}

function asArray(value: unknown, label: string): unknown[] {
  if (!Array.isArray(value)) {
    throw new Error(`${label} was missing or not an array.`);
  }

  return value;
}

function asRecord(value: unknown): Record<string, any> {
  if (value === null || typeof value !== "object") {
    return {};
  }

  return value as Record<string, any>;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
