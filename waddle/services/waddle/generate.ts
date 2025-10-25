import { WaddleDataService } from "../../../generators/projen-data-service";

const service = new WaddleDataService({
  serviceName: "waddle",
  databaseId: "",
  includeWriteModel: false,
});

service.synth();
