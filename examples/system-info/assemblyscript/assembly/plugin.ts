import { JSON } from "json-as";
import {
  Configuration,
  PortableState,
  hasConfigurationFields,
  validConfiguration,
} from "./config";
import {
  CATEGORY,
  definition,
  descriptor,
  failure,
  identity,
  save,
} from "./replies";

// Typed fields keep the example readable. The host also supplies company and
// paging fields, which this local system-info plugin does not need to read.
@json
class Request {
  abi: u32 = 0;
  operation: string = "";
  category: string = "";
  identity: JSON.Obj | null = null;
  state: JSON.Obj = new JSON.Obj();
  response: JSON.Obj | null = null;
  error: JSON.Obj | null = null;
}

export function dispatch(input: string): string {
  const request = JSON.parse<Request>(input);
  return JSON.stringify(handle(request));
}

function handle(request: Request): JSON.Obj {
  if (
    request.abi != 1 ||
    (request.operation != "descriptor" && request.category != CATEGORY)
  ) {
    return failure(
      "PLUGIN_INCOMPATIBLE",
      "The ABI or collection does not match this plugin.",
    );
  }

  let state = new PortableState();
  const saved = request.state.get("_pluginState");
  if (saved != null && saved.type != JSON.Types.Null) {
    if (saved.type != JSON.Types.Object) {
      return failure(
        "INVALID_PLUGIN_STATE",
        "Portable state must be an object.",
      );
    }
    const fields = saved.get<JSON.Obj>();
    const config = fields.get("config");
    if (
      (fields.keys().length != 3 && fields.keys().length != 4) ||
      !fields.has("schemaVersion") ||
      !fields.has("status") ||
      config == null ||
      config.type != JSON.Types.Object ||
      !hasConfigurationFields(config.get<JSON.Obj>())
    ) {
      return failure(
        "INVALID_PLUGIN_STATE",
        "The portable state does not match this example's schema.",
      );
    }
    state = JSON.parse<PortableState>(JSON.stringify(saved));
    if (state.schemaVersion != 1 || !validConfiguration(state.config)) {
      return failure(
        "INVALID_PLUGIN_STATE",
        "The portable state version or choice is invalid.",
      );
    }
  }

  switch (request.operation) {
    case "descriptor":
      return descriptor(state.config, state.selected);
    case "configure":
      return configure(request, state);
    case "discover": {
      if (!state.config.enabled) return disabled();
      const result = new JSON.Obj();
      result.set("type", "inventory");
      const identities = new JSON.Arr();
      identities.push(identity("SystemInfo"));
      result.set("identities", identities);
      result.set("nextCursor", JSON.Value.empty());
      result.set("authoritative", true);
      result.set("diagnostics", new JSON.Arr());
      return result;
    }
    case "acquire":
      if (!state.config.enabled) return disabled();
      return gather(request, state);
    default:
      return failure(
        "UNSUPPORTED_OPERATION",
        "This example does not expose that operation.",
      );
  }
}

function configure(request: Request, state: PortableState): JSON.Obj {
  const values = request.state.get("values");
  if (
    values == null ||
    values.type != JSON.Types.Object ||
    !hasConfigurationFields(values.get<JSON.Obj>())
  ) {
    return failure(
      "INVALID_PLUGIN_SETTINGS",
      "Supply the four declared setting values.",
    );
  }
  const config = JSON.parse<Configuration>(JSON.stringify(values));
  if (!validConfiguration(config)) {
    return failure(
      "INVALID_PLUGIN_SETTINGS",
      "Choose alpha or beta for the demo choice.",
    );
  }
  state.config = config;
  const selected = request.state.get("selected");
  if (selected != null && selected.type == JSON.Types.Bool) {
    state.selected = selected.get<bool>();
  }
  state.status.phase = config.enabled ? "ready" : "disabled";
  return save(
    state,
    definition(
      "PluginConfiguration",
      "plugin-configuration",
      JSON.Value.from(config),
    ),
  );
}

function gather(request: Request, state: PortableState): JSON.Obj {
  // Compare identity fields directly; JSON object property order has no meaning.
  if (request.identity != null) {
    const selected = request.identity!;
    const scope = selected.get("scope");
    const keys = selected.get("keys");
    if (
      selected.getAs<string>("category") != CATEGORY ||
      selected.getAs<string>("kind") != "SystemInfo" ||
      scope == null ||
      scope.type != JSON.Types.Object ||
      scope.get<JSON.Obj>().getAs<string>("type") != "global" ||
      keys == null ||
      keys.type != JSON.Types.Object ||
      keys.get<JSON.Obj>().keys().length != 0
    ) {
      return failure(
        "INVALID_IDENTITY",
        "Select this example's system-info identity.",
      );
    }
  }
  if (request.error != null) {
    state.status.phase = "failed";
    const result = new JSON.Obj();
    result.set("type", "failure");
    result.set("diagnostic", request.error!);
    return save(state, result);
  }
  if (request.response == null) {
    state.status.phase = "collecting";
    const action = new JSON.Obj();
    action.set("type", "systemInfo");
    const continuation = new JSON.Obj();
    continuation.set("phase", "systemInfo");
    const result = new JSON.Obj();
    result.set("type", "action");
    result.set("action", action);
    result.set("state", continuation);
    return save(state, result);
  }
  if (request.state.getAs<string>("phase") != "systemInfo") {
    return failure(
      "INVALID_PLUGIN_STATE",
      "The system-info continuation is missing.",
    );
  }
  if (state.status.runCount == u64.MAX_VALUE) {
    return failure(
      "STATUS_COUNTER_OVERFLOW",
      "The status counter exceeds its u64 format.",
    );
  }
  const report = new JSON.Obj();
  report.set("greeting", "Hello from Herbert!");
  report.set("facts", request.response!);
  state.status.phase = "complete";
  state.status.runCount += 1;
  state.status.lastReport = report;
  return save(
    state,
    definition("SystemInfo", "system-info", JSON.Value.from(report)),
  );
}

function disabled(): JSON.Obj {
  return failure(
    "COLLECTION_DISABLED",
    "Enable this example before gathering system info.",
  );
}
