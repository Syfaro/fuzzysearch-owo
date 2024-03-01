import * as echarts from "echarts";

document.getElementById("inject-job")?.addEventListener("click", (ev) => {
  if (!confirm("Are you sure you want to inject this job?")) {
    ev.preventDefault();
  }
});

interface IngestStatUpdate {
  timestamp: number;
  counts: Record<string, number>;
}

const ingestStatElement = document.getElementById("ingest-stats");

function ingestStatsGraph(elem: HTMLElement) {
  let pointDates: Array<Date> = [];
  let series: Record<string, any> = {};
  let previousValues: Record<string, number> = {};
  let addedPoints = -1;

  const chart = echarts.init(elem);

  const opts = {
    tooltip: {
      trigger: "axis",
    },
    toolbox: {
      feature: {
        dataZoom: {
          yAxisIndex: "none",
        },
        restore: {},
        saveAsImage: {},
      },
    },
    xAxis: {
      type: "category",
      boundaryGap: false,
      data: pointDates,
    },
    yAxis: {
      name: "Additions",
      type: "value",
      boundaryGap: [0, "100%"],
    },
    series: [],
  };

  chart.setOption(opts);

  const evtSource = new EventSource("/api/ingest/stats");

  evtSource.onmessage = (e) => {
    const data: IngestStatUpdate = JSON.parse(e.data);

    for (let [key, value] of Object.entries(data["counts"])) {
      if (series[key] === undefined) {
        series[key] = {
          name: key,
          type: "line",
          data: Array(addedPoints + 1).fill(0),
        };
      }

      series[key]["data"].push(value - (previousValues[key] || 0));
      previousValues[key] = value;
    }

    pointDates.push(new Date(data["timestamp"] * 1000));
    addedPoints += 1;

    chart.setOption({
      xAxis: {
        data: pointDates,
      },
      series: Object.values(series),
    });
  };
}

if (ingestStatElement) {
  ingestStatsGraph(ingestStatElement);
}
