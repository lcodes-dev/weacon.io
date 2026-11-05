import { writeFileSync } from "node:fs";

import { TYPES, currentTypes } from "./generate-types.ts";

writeFileSync(TYPES, currentTypes());
