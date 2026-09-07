# 0.1.1 사용성 개선과 검증

0.1.0 실행기를 사용한 개발 과정에서 발견한 문제를 후보 코드·회귀 테스트로 연결했다.
질문은 한 문장, 선택지는 결과별 label, 상세 근거는 계획 문서에 둔다.

| 관찰 | 원인 | 개선 | 회귀 근거 |
|---|---|---|---|
| 질문에 선택지·추천 근거가 중복됨 | UI 본문과 상세 증거가 한 문자열에 결합 | 질문 카드와 계획 문서 분리 | `dialogue`·`render` 테스트 |
| 승인 계획을 특정 영어 문구로 다시 전달해야 함 | inline 본문만 승인 인계로 인식 | 원문 요청·승인 참조·두 digest 결속 | `delegation_flow::t04_referenced_approval_*` |
| 승인에 붙인 범위 제한이 전체 거절로 처리됨 | 요청 전체의 부정 문구 부분 검색 | 호스트가 판별한 승인 상태 사용, 단독 거절 문장만 추가 검사 | `delegation_flow::t04_referenced_approval_preserves_scope_restrictions` |
| 상태 조회마다 긴 brief가 반복됨 | 등록 이후에도 전체 요청 직렬화 | 최초 전체 brief, 이후 배정 정보와 immutable artifact 참조 | `delegation_flow::t16_native_request_*` |
| 연결 지연이 수행 제한 시간을 소비함 | offer부터 단일 timeout 적용 | 연결 제한과 등록 후 수행 제한 분리 | `native_surface::t14_execution_deadline_*` |
| 중단 후보가 고위험 사전 검증을 막음 | 실패 기록·정리·복구 경계의 증거 불일치 | 실패 기록 전에 후보 백업, 중단 확인 후 복원, 사용한 시도 유지 | `delegation_policy::final_failed_author_*` |
| 모델은 가능하지만 검토자를 확보할 수 없음 | 작성자만 가용성 검사 | 작성 전에 Critic·Auditor의 역량·effort·슬롯 확인 | `delegation_policy::author_available_without_*`, `author_waits_when_*` |
| 모델 교체 후 PR 검토가 막힘 | 최종 검토의 고정 모델명 조건 | 카탈로그 검증과 역할·identity 독립성 검사 | `independent_catalog_review::arbitrary_catalog_model_*` |
| 작성 중 패키지에서 소스·바이너리 버전이 다름 | 패키징이 고정된 `HEAD`를 사용 | 체크포인트 ref를 명시하고 해당 snapshot의 버전 검사 | `tests/package.sh RUST_TARGET SNAPSHOT_REF` |

정리 중단 테스트는 실제 native 작성 실패 두 번 뒤 파일 시스템 정리 오류를 주입한다.
추적 파일은 복원되고 새 파일이 남는 상태에서 재시작하며, 실제 런타임이 먼저 만든 백업을 사용한다.
수정 전에는 고위험 검증 오류를 재현했고 수정 후에는 추가 작성 없이 예산 소진 상태에 도달한다.

## 작업 단위와 전체 흐름

[7개 작업 단위](delegation-work-items.md)의 T01–T17 책임 구분을 유지한다.
`delegation_flow`는 PLAN·직접 BUILD·승인 계획 가져오기의 정상·거부 경로를 검사한다.
정상 경로는 ADJUST, 의존 작업 재검증, 전체 테스트, 양측 PR 검토와 현재 후보의 SHIP까지 수행한다.
Native fixture는 실제 요청의 모델·effort·agent ID·decision digest를 검사한다.
모델의 응답 품질과 청구 비용은 호스트 실행 관측으로 평가하며 미계측 항목은 `unknown`이다.

## 체크포인트와 복구 이력

200줄 체크포인트는 검증한 작업 파일만 stage하고 `commit-tree`로 별도 checkpoint ref에 보관한다.
`refs/hwahap-checkpoints/`는 중간 후보를 보존하며 실행 브랜치 HEAD는 런타임의 unit acceptance가 갱신한다.

두 시도를 소진한 run은 실패 지적·원본 checkout·검토 기록을 보존한다.
복구 작업은 승인 범위와 남은 지적을 명시하고 마지막 accepted commit을 기준으로 시작한다.
이전 실패 기록과 새 성공 기록은 각각의 run에 남긴다.

## 출시 검증

후보에서 fmt, Clippy, 전체 Cargo 테스트, 버전 일치와 정적 게이트를 실행한다.
macOS Apple Silicon에서 0.1.1 archive·단일 바이너리와 Codex CLI 0.153.4의 격리 설치·실제 MCP 연결을 검증했다.
태그 배포는 GitHub Actions에서 세 플랫폼의 빌드·패키지·실제 Codex plugin 설치를 검증한 뒤 공개한다.
