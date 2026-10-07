PACKAGE order_pkg IS
  -- 주문 합계 (할인 포함)
  FUNCTION calc_total(p_id IN NUMBER) RETURN NUMBER;
  -- 주문을 닫고 이력을 남긴다
  PROCEDURE close_order(p_id NUMBER);
  PROCEDURE retry(p_n NUMBER);
  -- 밤 배치
  PROCEDURE nightly;
END order_pkg;
