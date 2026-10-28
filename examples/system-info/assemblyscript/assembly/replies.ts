import { JSON } from "json-as";
import { Configuration, PortableState } from "./config";

export const CATEGORY: string = "Example.SystemInfo.AssemblyScript";
const DEMO_NOTE: string =
  "Demo / example — no effect. This value is saved but does not change system info.";

export function identity(kind: string): JSON.Obj {
  const scope = new JSON.Obj();
  scope.set("type", "global");
  const result = new JSON.Obj();
  result.set("category", CATEGORY);
  result.set("kind", kind);
  result.set("scope", scope);
  result.set("keys", new JSON.Obj());
  return result;
}

export function definition(
  kind: string,
  format: string,
  value: JSON.Value,
): JSON.Obj {
  const result = new JSON.Obj();
  result.set("type", "definition");
  result.set("identity", identity(kind));
  result.set("format", format);
  result.set("definition", value);
  result.set("completeness", "collected");
  result.set("diagnostics", new JSON.Arr());
  return result;
}

export function save(state: PortableState, reply: JSON.Obj): JSON.Obj {
  const result = new JSON.Obj();
  result.set("type", "state");
  result.set("pluginState", state);
  result.set("reply", reply);
  return result;
}

export function failure(code: string, message: string): JSON.Obj {
  const diagnostic = new JSON.Obj();
  diagnostic.set("code", code);
  diagnostic.set("message", message);
  diagnostic.set("retryable", false);
  const result = new JSON.Obj();
  result.set("type", "failure");
  result.set("diagnostic", diagnostic);
  return result;
}

// These helpers build the same three generic controls as the Rust example.
// They describe presentation and defaults. The plugin owns each value's meaning.
function checkbox(
  key: string,
  label: string,
  value: bool,
  demo: bool,
): JSON.Obj {
  const result = new JSON.Obj();
  result.set("kind", "checkbox");
  result.set("key", key);
  result.set("label", label);
  result.set(
    "description",
    demo
      ? DEMO_NOTE
      : "Allow this plugin to gather the host's curated system info.",
  );
  result.set("default", value);
  result.set("demoOnly", demo);
  return result;
}

function option(label: string, value: string): JSON.Obj {
  const result = new JSON.Obj();
  result.set("label", label);
  result.set("value", value);
  return result;
}

function choice(value: string): JSON.Obj {
  const result = new JSON.Obj();
  result.set("kind", "choice");
  result.set("key", "demoChoice");
  result.set("label", "Demo choice");
  result.set("description", DEMO_NOTE);
  result.set("default", value);
  result.set("demoOnly", true);
  const options = new JSON.Arr();
  options.push(option("Alpha", "alpha"));
  options.push(option("Beta", "beta"));
  result.set("options", options);
  return result;
}

function filter(value: string): JSON.Obj {
  const result = new JSON.Obj();
  result.set("kind", "filter");
  result.set("key", "demoFilter");
  result.set("label", "Demo filter");
  result.set("description", DEMO_NOTE);
  result.set("default", value);
  result.set("demoOnly", true);
  return result;
}

// Help is plain text in the descriptor. The host displays it but does not own
// its meaning. Build dynamic JSON arrays explicitly, as with the settings.
function helpSection(title: string, paragraphs: string[]): JSON.Obj {
  const section = new JSON.Obj();
  section.set("title", title);
  const text = new JSON.Arr();
  for (let index = 0; index < paragraphs.length; index++) {
    text.push(paragraphs[index]);
  }
  section.set("paragraphs", text);
  return section;
}

function help(): JSON.Arr {
  const sections = new JSON.Arr();
  sections.push(
    helpSection("What this example does", [
      "This example greets you and reports Herbert's version, plugin interface version, operating system, processor architecture, pointer size, byte order and available parallelism.",
      "These facts describe the computer that runs Herbert. The module gathers them during a download and makes no Epicor reads. It does not read credentials, user files or business data.",
    ]),
  );
  sections.push(
    helpSection("Options and portable state", [
      "Enable and select this example to include local system facts in a download. The example starts disabled.",
      "Demo checkbox, Demo choice and Demo filter are examples only. Their values are saved, but they do not change the system facts or select data.",
      "Save the options to keep configuration beside the module. Download progress reports execution status. Move the state file with the module to retain its configuration and history.",
    ]),
  );
  return sections;
}

export function descriptor(config: Configuration, selected: bool = true): JSON.Obj {
  const category = new JSON.Obj();
  category.set("id", CATEGORY);
  category.set("name", "System info (AssemblyScript example)");
  category.set(
    "description",
    "Hello-world example: curated host facts, portable configuration and status.",
  );
  category.set("pluginVersion", "0.1.0");
  category.set("available", config.enabled);
  category.set("selected", selected);
  category.set(
    "qualification",
    "Teaching example; system facts describe the Herbert host.",
  );
  // Dynamic JSON fields need JSON.Arr. Keep ordinary AssemblyScript arrays for
  // ordinary program data; the JSON library does not box them automatically.
  const roles = new JSON.Arr();
  roles.push("collector");
  roles.push("local");
  category.set("roles", roles);
  category.set("helpers", new JSON.Arr());
  category.set("help", help());
  const settings = new JSON.Arr();
  settings.push(
    checkbox("enabled", "Enable this example", config.enabled, false),
  );
  settings.push(
    checkbox("demoCheckbox", "Demo checkbox", config.demoCheckbox, true),
  );
  settings.push(choice(config.demoChoice));
  settings.push(filter(config.demoFilter));
  category.set("settings", settings);
  const result = new JSON.Obj();
  result.set("type", "descriptor");
  result.set("abi", <u32>1);
  result.set("category", category);
  result.set("methods", new JSON.Arr());
  return result;
}
