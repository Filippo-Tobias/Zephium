if (!["darwin", "win32", "linux"].includes(process.platform)) {
  console.error(
    `Zephium does not support ${process.platform}.`,
  );
  process.exit(1);
}
