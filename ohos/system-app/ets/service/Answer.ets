// Innkeeper User's answer to a request from Innkeeper System
// (ets/service/PerUser.ets), which it sends in the want parameters of a start
// of Innkeeper System's service. This module uses no OpenHarmony API, so its
// tests run under Node.
export class Answer {
  request: number = 0;
  error: string = '';
  reply: string = '';
}

// Reads Innkeeper User's answer from a want's parameters, or returns
// undefined if they hold none.
export function userAnswer(p: Record<string, Object> | undefined): Answer | undefined {
  if (p === undefined) {
    return undefined;
  }
  const request = p['innkeeper.answer.request'];
  const error = p['innkeeper.answer.error'];
  const reply = p['innkeeper.answer.reply'];
  if (typeof request !== 'number' || typeof error !== 'string' || typeof reply !== 'string') {
    return undefined;
  }
  const a = new Answer();
  a.request = request as number;
  a.error = error as string;
  a.reply = reply as string;
  return a;
}
