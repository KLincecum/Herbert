import { JSON } from "json-as";

// Only enabled changes behavior. The three demo values are saved as examples.
@json
export class Configuration {
  enabled: bool = false;
  demoCheckbox: bool = false;
  demoChoice: string = "alpha";
  demoFilter: string = "";
}

@json
export class Status {
  phase: string = "disabled";
  runCount: u64 = 0;
  lastReport: JSON.Obj | null = null;
}

// This value travels beside the .wasm file. The host stores it without knowing
// what these fields mean. A future format must use a different schemaVersion.
@json
export class PortableState {
  schemaVersion: u32 = 1;
  selected: bool = true;
  config: Configuration = new Configuration();
  status: Status = new Status();
}

export function validConfiguration(config: Configuration): bool {
  return config.demoChoice == "alpha" || config.demoChoice == "beta";
}

// Check the wire shape before typed parsing. This prevents a misspelled setting
// from being silently replaced by a default.
export function hasConfigurationFields(values: JSON.Obj): bool {
  if (values.keys().length != 4) return false;
  const enabled = values.get("enabled");
  const checkbox = values.get("demoCheckbox");
  const choice = values.get("demoChoice");
  const filter = values.get("demoFilter");
  return (
    enabled != null &&
    enabled.type == JSON.Types.Bool &&
    checkbox != null &&
    checkbox.type == JSON.Types.Bool &&
    choice != null &&
    choice.type == JSON.Types.String &&
    filter != null &&
    filter.type == JSON.Types.String
  );
}
