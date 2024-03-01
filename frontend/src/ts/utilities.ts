import { toast } from "bulma-toast";

const dtf = new Intl.DateTimeFormat("en", {
  timeStyle: "medium",
  dateStyle: "medium",
});
const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

const units: Record<string, number> = {
  year: 24 * 60 * 60 * 1000 * 365,
  month: (24 * 60 * 60 * 1000 * 365) / 12,
  day: 24 * 60 * 60 * 1000,
  hour: 60 * 60 * 1000,
  minute: 60 * 1000,
  second: 1000,
};

function getRelativeTime(toDate: Date, fromDate = new Date()): string {
  const elapsed = +toDate - +fromDate;

  for (const unit in units) {
    if (Math.abs(elapsed) > units[unit] || unit === "second") {
      return rtf.format(
        Math.round(elapsed / units[unit]),
        unit as Intl.RelativeTimeFormatUnit,
      );
    }
  }

  throw new Error("unable to find unit for relative time");
}

function updateRelativeTimes() {
  document
    .querySelectorAll<HTMLElement>(".relative-time[data-timestamp]")
    .forEach((elem) => {
      if (!elem.dataset.timestamp) return;

      const timestamp = parseInt(elem.dataset.timestamp, 10);
      const date = new Date(timestamp * 1000);

      if (!elem.dataset.replacedText) {
        elem.dataset.tooltip = dtf.format(date);
        elem.dataset.replacedText = "true";
      }

      elem.textContent = getRelativeTime(date);
    });

  setTimeout(updateRelativeTimes, 1000 * 15);
}

updateRelativeTimes();

document
  .querySelectorAll<HTMLElement>(".absolute-time[data-timestamp]")
  .forEach((elem) => {
    if (!elem.dataset.timestamp) return;

    const timestamp = parseInt(elem.dataset.timestamp, 10);
    const date = new Date(timestamp * 1000);

    elem.textContent = dtf.format(date);
  });

document
  .querySelectorAll<HTMLInputElement>("input[type=file]")
  .forEach((input) => {
    input.addEventListener("change", () => {
      if (!input.dataset.uploadButton) return;
      const uploadButton = document.querySelector(input.dataset.uploadButton);

      if (input.files?.length === 0) {
        uploadButton?.setAttribute("disabled", "disabled");
        return;
      }

      if (!input.dataset.fileNameLabel) return;
      const fileName = document.querySelector(input.dataset.fileNameLabel);

      if (fileName) {
        const displayedName = input.files?.length === 1
          ? input.files[0].name
          : `${input.files?.length} Files Selected`;
        fileName.textContent = displayedName;
      }

      uploadButton?.removeAttribute("disabled");
    });
  });

document.querySelectorAll<HTMLElement>(".navbar-burger").forEach((burger) => {
  burger.addEventListener("click", () => {
    if (!burger.dataset.target) return;
    const target = document.querySelector(burger.dataset.target);

    [burger, target].forEach((el) => el?.classList.toggle("is-active"));
  });
});

document
  .querySelectorAll<HTMLFormElement>(".chunk-uploader")
  .forEach((chunkUploader) => {
    chunkUploader.addEventListener("submit", (ev) => {
      ev.preventDefault();

      window.onbeforeunload = () => {
        return "Archive is uploading";
      };

      const fileInput = <HTMLInputElement> (
        chunkUploader.querySelector('input[type="file"]')
      );
      if (!fileInput) return;

      const file = fileInput.files?.[0];
      if (!file) return;

      chunkUploader
        .querySelector(".upload-button")
        ?.classList.add("is-loading");

      const progressBar = chunkUploader.querySelector("progress");
      progressBar?.classList.remove("is-hidden");

      performChunkedUpload(file, progressBar)
        .then((collectionId) => {
          window.onbeforeunload = null;
          console.log(`Completed uploading chunks to ${collectionId}`);

          let collectionIDElement = <HTMLInputElement> (
            chunkUploader.querySelector('input[name="collection_id"]')
          );
          if (collectionIDElement) {
            collectionIDElement.value = collectionId;
          }

          fileInput.value = "";

          chunkUploader.submit();
        })
        .catch((err) => {
          window.onbeforeunload = null;
          console.error(err);

          alert(`Upload failed: ${err}`);
          window.location.reload();
        });
    });
  });

async function performChunkedUpload(
  file: File,
  progressBar: HTMLProgressElement | null,
) {
  const CHUNK_SIZE = 1024 * 1024 * 10;

  const collectionId = window.crypto.randomUUID();
  const fileSize = file.size;

  const totalChunks = Math.ceil(fileSize / CHUNK_SIZE);
  let currentChunk = 1;

  while (currentChunk <= totalChunks) {
    console.debug(`Uploading chunk ${currentChunk}`);

    if (progressBar) {
      progressBar.value = currentChunk;
      progressBar.max = totalChunks;
    }

    const offset = (currentChunk - 1) * CHUNK_SIZE;
    const filePart = file.slice(offset, offset + CHUNK_SIZE);

    const formData = new FormData();
    formData.set("chunk", filePart);

    const resp = await fetch(`/api/chunk/${collectionId}/add`, {
      method: "POST",
      body: formData,
      credentials: "same-origin",
    });

    if (resp.status !== 200) {
      throw new Error("bad status code");
    }

    currentChunk++;
  }

  return collectionId;
}

document.querySelectorAll<HTMLInputElement>(".click-copy").forEach((elem) => {
  elem.addEventListener("click", () => {
    navigator.clipboard.writeText(elem.value);

    toast({
      message: "Copied value.",
      type: "is-info",
    });
  });
});

document.querySelectorAll<HTMLInputElement>(".hover-reveal").forEach((elem) => {
  elem.addEventListener("mouseover", () => {
    elem.type = "text";
  });

  elem.addEventListener("mouseleave", () => {
    elem.type = "password";
  });
});

const numberFormatter = new Intl.NumberFormat(undefined, {
  style: "decimal",
});

document.querySelectorAll(".human-number").forEach((elem) => {
  const value = parseInt(elem.textContent?.trim() || "0", 10);
  elem.textContent = numberFormatter.format(value);
});
