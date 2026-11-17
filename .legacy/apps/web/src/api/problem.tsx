/** Why a request failed, with Try again when it can be asked again. */
export function Problem({ text, retry }: { text: string; retry?: () => void }) {
  return (
    <p role="alert">
      {text}
      {retry !== undefined && (
        <>
          {" "}
          <button type="button" onClick={retry}>
            Try again
          </button>
        </>
      )}
    </p>
  );
}
