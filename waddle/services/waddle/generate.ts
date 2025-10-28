import { WaddleDataService } from "../../../generators/projen-data-service";

const service = new WaddleDataService({
  serviceName: "waddle",
  databaseId: "6c09af2c-2665-4a56-a805-09c40b5e0711",
  includeWriteModel: false,
});

service.synth();
