# PLAN·BUILD 요청과 실행 사용량

## 계획과 구현을 따로 요청하기

Hwahap의 PLAN은 제품 결정을 정리하는 작업 단계다. Codex의 collaboration mode를 바꾸는 명령이 아니다.
모든 `hwahap_step` 호출에는 같은 저장소의 `cwd`와 부모의 `host_session_id`를 함께 보낸다.
아래 JSON의 `<...>` 값은 실제 출력값으로 교체한다.

| 사용자 의도 | `hwahap_step` 입력 | 도달 상태 |
|---|---|---|
| 계획만 만들기 | `request`와 `plan_only:true` | 사용자 `CONFIRM PLAN` 이후 `plan_ready` |
| 기획부터 구현까지 | `request`; `plan_only` 기본값은 `false` | 사용자 `CONFIRM PLAN` 이후 구현·draft 검토 |
| Codex에서 이미 승인한 계획 구현하기 | `approved_plan` | 승인 원문 저장 → 독립 변환 검토 → 추가 승인 없이 BUILD |
| 확정된 계획 구현하기 | `build_confirmed:"<전체 plan_digest>"` | 저장된 계약을 검증하고 BUILD 재개 |
| 기획을 명시적으로 생략하기 | `build`에 `BuildRequest` | 원문 실행 권한과 별도 고정 계약으로 direct BUILD |
| 같은 계약의 구현 수정하기 | `adjust_build` | 지정한 unit의 기존 계약으로 수정·검증 |
| 요구사항·계약 바꾸기 | 사용자 원문 `user_input` | PLAN에서 결정·계약을 다시 검토 |

`build_confirmed`는 사용자가 저장된 계획의 구현을 명시적으로 요청한 뒤에만 전달한다.
값은 출력된 전체 digest이며 `CONFIRM PLAN`에 쓰는 짧은 challenge와 다르다.
PLAN-only의 `plan_ready`에서 반복 조회하거나 `hwahap_step`을 빈 입력으로 호출해도 구현 권한을 추가하지 않는다.
`build`의 `user_instruction`, `objective`, `base_branch`, `branch`, `units`, `full_suite`는
기획 생략을 명시한 새 실행 계약이다. 확정된 PLAN을 실행하는 `build_confirmed`와 구분한다.

```json
{
  "cwd": "/absolute/repository",
  "host_session_id": "<현재 부모 ID>",
  "adjust_build": {
    "user_instruction": "기존 계약은 유지하고 U1 구현 오류를 수정해 줘.",
    "contract_digest": "<현재 전체 contract_digest>",
    "unit_ids": ["U1"]
  }
}
```

`adjust_build`에는 사용자의 수정 권한 원문과 현재 계약 digest, 대상 unit ID를 넣는다.
acceptance·테스트·허용 경로를 바꾸는 입력은 이 경로로 전달하지 않는다. 그런 변경은 `user_input`으로
PLAN에 돌아가 결정하고 다시 확인한다. 기획 생략 권한이 이후 계약 변경까지 승인한 것은 아니다.

## Codex에서 승인한 계획 넘기기

사용자가 `PLEASE IMPLEMENT THIS PLAN:` 뒤에 전체 계획을 제출했다면 이미 받은 구현 승인을 보존한다.
Hwahap에 그 계약이 없다는 이유로 기획 생략용 `build`를 쓰거나 `CONFIRM PLAN`을 다시 요구하지 않는다.
호스트는 `approved_plan`을 지원하는 실제 도구 스키마를 확인하고 다음을 전달한다.
같은 승인 메시지를 일반 `request`·`user_input`으로 보내면 인터뷰를 시작하지 않고 인계 경로를 안내한다.

- `approval.implementation_request`: 사용자의 전체 실제 메시지. 요약·동의 답변으로 대신 만들지 않는다.
- `approval.markdown`: 접두사 다음 전체 계획 본문. 바깥 공백만 제거한다.
- `approval.markdown_digest`: 그 본문 UTF-8 바이트의 SHA-256, `sha256:<hex>` 형식.
- `approval.source_head`: 변환에 사용한 현재 깨끗한 checkout의 정확한 commit.
- `contract`: `BuildRequest` 형식의 실행 명세. 원문과 같은 `user_instruction`, 목표·기준 브랜치·
  새 `codex/` 브랜치·전체 테스트 명령·작업별 수용 기준·허용 경로·테스트를 빠짐없이 담는다.
- `replaces_plan_digest`: 기존 미실행 초안이 있으면 현재 전체 digest, 없으면 `null`.

등록은 승인 원문과 이전 초안 이력을 journal에 함께 보존하고 `proving`에 진입한다. 이전 인터뷰 답변이나
검토 통과를 만들어 넣지 않는다. 독립 검토자 둘은 원문과 전체 실행 계약을 비교한다. 변환된 명세의 누락·
권한 확대·새 결정을 구현 전에 검토하도록 요구한다. 검토 통과는 의미적 동등성이나 모든 오해의 제거를 증명하지 않는다.

변환 결함의 `plan_conflict/repair_translation`에서는 승인 원문을 그대로 유지하고 명세만 수정해 `approved_plan`을 다시 보낸다.
`replaces_plan_digest`는 실패한 현재 초안의 digest로 갱신한다. 무관한 사용자 답변으로 승인 기록을 버리지 않는다.
새로운 중요한 선택이 필요할 때만 사용자에게 그 차이를 묻는다. 원문의 승인 범위를 늘리면 안 된다.

검토가 끝나면 사용자 원문을 실행 권한으로 보존하고 BUILD로 이어간다. 시작 직전 중단으로 `plan_ready`에
남더라도 `next:continue`를 따라 추가 승인 없이 재개한다. PLAN-only의 `plan_ready/await_user`와 구분한다.
동일 등록 재시도는 같은 run을 반환하며, 다른 부모 작업·stale 초안·frozen/executing 계약 교체는 거부한다.
이 경로는 별도 `SHIP`, merge 또는 배포 권한을 만들지 않는다. 실제 호스트가 전달한 메시지의 바이트 결속이며,
런타임이 사용자 신원을 독립적으로 인증한 것은 아니다.

## 질문 UI와 원문 응답

`question_batch`는 현재 계획에 결속된 `batch_id`와 최대 3개의 `questions`를 반환한다.
각 질문은 `id`, 전체 `question`, `options`의 정확한 `label`·`description`을 가진다.
추천 대안은 첫 번째에 `(Recommended)`로 표시되며, `UNKNOWN`은 결정 보류다.
영역 제외 제안에는 제외와 적용 유지 선택지가 있다. 적용 유지를 선택하면 제외 제안을 철회한다.

호스트는 실제 호출 가능한 `request_user_input` 또는 `request_user_input_async`를 확인한다.
`request_user_input`은 호스트에 따라 Plan 모드에서만 사용할 수 있으며 한 번에 1–3개 질문 같은
제한이 있다. `request_user_input_async`도 노출된 경우에만 사용한다. 없는 도구 이름을 만들거나
질문 도구를 쓰기 위해 Codex 모드를 바꾸지 않는다. 정확한 전달 절차는 MCP `instructions`를 따른다.

질문 본문·대안·추천 근거를 모두 그대로 표시한다. 도구가 지원하는 선택지 수보다 많으면 일부를
지우지 말고, 전체 label을 표시한 자유입력 UI에서 사용자가 선택한 label을 그대로 입력하게 한다.
자유입력 UI도 지원하지 않으면 전체 질문을 텍스트로 표시하고 한계를 알린다. 사용자가 답한
질문 ID와 원문을 아래 구조로 보존하며, 부모가 의미를 보충하거나 승인 문법으로 다시 쓰지 않는다.

```json
{
  "question_response": {
    "batch_id": "<question_batch.batch_id 그대로>",
    "responses": [
      {"id": "C1", "answer": "<사용자가 선택한 정확한 label 또는 자유입력 원문>"}
    ]
  }
}
```

현재 배치 밖 질문·중복 ID·오래된 배치는 전체 응답을 거부한다. 빈 응답·취소·무응답은 선택을 만들지 않는다.
정확한 label과 일치하지 않는 자유입력은 `Clarify`로 보존한다. 명령처럼 보이는 문자열도 실행하거나
다른 질문의 답으로 해석하지 않는다. 결정의 자유입력은 새 해석 선택지에 답할 때까지 미해결로 두며,
해석을 만들지 못하면 `plan_conflict`에서 보충 입력을 기다린다. 영역 질문은 원문을 함께 표시하고
적용·제외를 다시 선택하게 한다. UI의 기본값 표시만으로 답을 만들지 않으며, 사용자가 실제 제출한
추천 선택지 응답은 허용한다.
`CONFIRM PLAN`과 `SHIP`은 질문 UI에 넣지 않으며, 사용자가 입력한 정확한 문장을 별도로 전달한다.

[공식 App Server 문서](https://learn.chatgpt.com/docs/app-server#api-overview)는 실험 기능
`tool/requestUserInput`의 1–3개 질문을 설명한다. [응답 수명주기](https://learn.chatgpt.com/docs/app-server#toolrequestuserinput)에
따르면 `serverRequest/resolved`는 실제 답변과 요청 정리 모두에서 발생하고 자동 종료도 가능하다.
따라서 UI 닫힘·timeout·resolved 이벤트만으로 답변이나 승인을 기록하지 않는다.
이 인터페이스는 호스트가 전달한 응답을 검증하며 사용자 신원을 암호학적으로 인증하는 장치는 아니다.

## 사용량 관측

Hwahap은 `.hwahap/usage.json`에 사용량 관측값과 관측 시점을 저장한다. `hwahap_step`,
`hwahap_ship`, 검토 보고서 갱신 및 `usage sync`가 저장하며 `hwahap_status`와 `usage show`는 읽기 전용이다.
run을 archive하면 사용량과 기준값도 함께 보존한다. `.hwahap`은 Git에 추가하지 않는다.

## 로컬 Codex 사용량 연결

현재 설치의 바이너리의 launcher에 아래 인자를 전달한다. 먼저 Hwahap run을 시작하고 해당 실행에
참여하는 부모·자식의 정확한 로컬 Codex session JSONL 경로를 연결한다. 전역 설정 변경은 없다.

```sh
bin/hwahap usage attach /absolute/repository /absolute/rollout-session.jsonl
bin/hwahap usage sync /absolute/repository
bin/hwahap usage show /absolute/repository
```

기본 attach는 **현재 누적값을 기준값으로 저장하고 이후 증가분만** 집계한다. 동일 attach를 다시
실행해도 기준값이 움직이지 않는다. 재사용한 부모·자식의 이전 run 사용량을 포함하지 않기 위해서다.
처음부터 해당 run만 수행한 새 자식의 전체 사용량은 attach 끝에 `--from-start`를 붙여 연결할 수 있다.
이 옵션은 기존 세션의 과거 작업·상속된 사용량도 포함할 수 있으므로 새 세션에만 사용한다.
호스트가 정확한 경로를 알 수 없으면 해당 사용량은 미계측으로 남긴다. 임의 숫자를 만들지 않는다.

수집기는 `session_meta`, `turn_context`, `event_msg/token_count`의 숫자만 추출한다. 원본 대화,
추론 본문, 도구 출력은 `.hwahap`에 복사하지 않는다. 현재 로그 형식은 로컬 파일 관찰에 기반한
호환 어댑터이며 공식적으로 고정된 API라고 주장하지 않는다. 같은 세션은 한 번만 집계하고,
중복 누적 이벤트·부분 기록은 이중 합산하지 않는다. 파일 교체·누적값 감소·누락·64 MiB 초과는
`unavailable_sessions`에 표시한다. 해당 세션을 0비용으로 해석하지 않는다.

`observed_session_usage`는 연결된 세션의 부모 전달·계획·실패·재사용 작업을 포함하는 관측값이다.
`total`/`by_requested_model`은 별도로 native completion이 보고한 dispatch 사용량이다.
두 집계는 **겹치므로 더하지 않는다**. 캐시 입력은 전체 입력의 부분이며 reasoning 출력도 이미
전체 출력에 포함된다. 미연결 세션, attach 이전 작업, 아직 기록되지 않은 이벤트는 빠져 있다.
모델별 값은 로그에 기록된 모델 문맥 기준이며 서비스의 실제 모델 적용이나 청구 증명은 아니다.

## 선택적 비용 추정

사용 계정의 단가를 확인한 뒤 `.hwahap/pricing.json`을 둔다. 예를 들어 아래는 2026-09-06에
확인한 [Codex Standard credit 표](https://learn.chatgpt.com/docs/pricing)의 텍스트 단가다.
Fast mode, 계약별 단가, 도구 요금, 긴 문맥 등의 추가 요금은 자동 적용하지 않는다.

```json
{
  "currency": "credits",
  "source": "https://learn.chatgpt.com/docs/pricing",
  "effective_date": "2026-09-06",
  "assumptions": "Standard text rates only; excludes speed, contract and other surcharges",
  "per_million": {
    "gpt-6-astra": {"input": 250, "cached_input": 25, "output": 1250},
    "gpt-5.6-terra": {"input": 50, "cached_input": 5, "output": 300},
    "gpt-5.6-luna": {"input": 5, "cached_input": 0.5, "output": 30}
  }
}
```

단위는 백만 토큰당 가격이다. cache write가 관측되면 해당 모델의 `cache_write_input` 단가도
필요하다. 알려지지 않은 모델·write 단가는 `unpriced_models`에 남긴다. 금액은
`cost_estimate.priced_subtotal`이며 **실제 전체 청구금액이 아니다**. 단가표가 없거나 잘못돼도
토큰 관측값은 유지한다. 원래 단가 출처·유효일·가정을 보고서에 함께 남긴다.

## 모델 배치 비교

기본 PLAN은 Luna 첫 구현, Astra 검토·재작업이다. 중간 난도 구현에 Terra를 평가하려면
새 부모 pool에서 `.hwahap/config.toml`에 아래처럼 세 profile을 명시한다. 기존 pool의 모델은
바꾸지 않는다. direct BUILD는 기존 계약대로 부모 Astra와 별도 Astra 검토자 둘을 사용한다.

```toml
[profiles.economy]
model = "gpt-5.6-terra"
effort = "medium"
[profiles.critic]
model = "gpt-6-astra"
effort = "high"
[profiles.deep]
model = "gpt-6-astra"
effort = "high"
```

동일한 요청·base commit·acceptance·테스트로 Luna와 Terra 실행을 비교한다. `evaluation`의
run 상태·통과 unit 수·수정 시도·요청 profile, `latency`, 계측 범위와 토큰·추정 비용을 함께 본다.
실패·중단된 실행 비용도 포함한 성공 작업당 비용을 비교하고, 검증 기준을 낮춰 절감을 만들지 않는다.
대표 과제와 회귀 반례로 품질 기준을 먼저 고정한다. 측정 없이 모델 이름만으로 최적 배치를 단정하지 않는다.

PR 공격·방어의 초기 brief에는 전체 diff 대신 정확한 base/head와 변경 경로 목록을 담는다.
두 검토자는 그 revision의 실제 diff와 필요한 주변 소스를 직접 읽어 근거를 작성한다.
경로 목록은 검토 증거가 아니며, 큰 diff의 앞부분을 잘라 전달해 검토했다고 간주하지 않는다.

공식 모델 설명은 [Astra](https://developers.openai.com/api/docs/models/gpt-6-astra),
[Terra](https://developers.openai.com/api/docs/models/gpt-5.6-terra),
[Luna](https://developers.openai.com/api/docs/models/gpt-5.6-luna)를 따른다.
[App Server](https://learn.chatgpt.com/docs/app-server)는 `thread/tokenUsage/updated`를 제공한다.
호스트가 직접 이 이벤트를 받을 수 있으면 실제 counters를 전달할 수 있지만, 현재 MCP 도구가
그 이벤트를 자동 구독한다고 가정하지 않는다. 수집·비교 방향은
[Uber의 측정·문맥 비용·작업별 평가](https://www.uber.com/us/en/blog/efficient-software-factory/)를 참고했다.
